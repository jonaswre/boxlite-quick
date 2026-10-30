//! Boots a real Box, so it needs Linux x86_64, KVM and BoxLite's host prerequisites. Run with
//! `cargo test --test real_box -- --ignored`; CI runs it on every push.
//!
//! boxq execs through the Box's own `box.sock` so no runtime (and no `BOXLITE_HOME` lock) is
//! held during a session (jonaswre/boxlite-quick#8). That leans on BoxLite internals: the
//! socket layout, the `BOXLITE_EXECUTOR` env protocol and the container-id directory. This
//! test pins them, so any BoxLite bump must pass it. Once boxq has its own lock-free exec
//! target (jonaswre/boxlite-quick#10), this test should drive that instead of BoxLite directly.

use boxlite::runtime::options::{BoxOptions, BoxliteOptions, RootfsSpec};
use boxlite::{BoxCommand, BoxliteRuntime, GuestSession, LiteBox};
use boxlite_shared::BoxTransport;
use boxq::reap::reap_box;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

const BOX_NAME: &str = "boxq-real-box-test";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_command_runs_through_box_sock_while_the_runtime_is_held() {
    let home = tempfile::tempdir().unwrap();
    let rt = runtime(home.path());
    let b = create(&rt).await;

    let ran = run_while_held(&b, home.path()).await;

    // Clean up before asserting, so a failure never leaves a VM running.
    clean_up(&rt, &b, home.path()).await;

    let (lock_held, stdout, exit_code) = ran.unwrap();
    assert!(lock_held, "the runtime should hold the BOXLITE_HOME lock");
    assert_eq!((stdout.as_str(), exit_code), ("hello from the box\n", 0));
}

/// BoxLite 0.11 leaks the shim and VM on `stop` wherever it can't create the Box's cgroup,
/// which includes GitHub's runners (jonaswre/boxlite-quick#19). Remove `reap_box` once a BoxLite
/// release reaps the tree itself (boxlite-ai/boxlite#1240), and this test still passes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn no_shim_survives_stop_or_rm_once_reaped() {
    let home = tempfile::tempdir().unwrap();
    let rt = runtime(home.path());
    let b = create(&rt).await;

    let survivors = stop_then_rm(&rt, &b, home.path()).await;

    clean_up(&rt, &b, home.path()).await;

    let (after_stop, after_rm) = survivors.unwrap();
    assert_eq!(after_stop, Vec::<PathBuf>::new(), "shims left after stop");
    assert_eq!(after_rm, Vec::<PathBuf>::new(), "shims left after rm");
}

/// Starts and stops the Box, then starts and force-removes it, reaping after each. Returns the
/// shims still running after each reap.
async fn stop_then_rm(
    rt: &BoxliteRuntime,
    b: &LiteBox,
    home: &Path,
) -> anyhow::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let id = b.id().to_string();
    b.start().await?;
    b.stop().await?;
    reap_box(home, &id)?;
    let after_stop = shims(home)?;

    // `stop` invalidates the handle.
    let b = rt
        .get(BOX_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the stopped Box is gone"))?;
    b.start().await?;
    rt.remove(BOX_NAME, true).await?;
    reap_box(home, &id)?;
    Ok((after_stop, shims(home)?))
}

fn runtime(home: &Path) -> BoxliteRuntime {
    BoxliteRuntime::new(BoxliteOptions {
        home_dir: home.into(),
        ..Default::default()
    })
    .unwrap()
}

async fn create(rt: &BoxliteRuntime) -> LiteBox {
    let options = BoxOptions {
        rootfs: RootfsSpec::Image("alpine:3".into()),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    rt.create(options, Some(BOX_NAME.into())).await.unwrap()
}

/// Starts the Box, then, with the runtime still open, runs `echo` in it through `box.sock`.
async fn run_while_held(b: &LiteBox, home: &Path) -> anyhow::Result<(bool, String, i32)> {
    b.start().await?;
    let box_dir = home.join("boxes").join(b.id().to_string());

    let lock_held = BoxliteRuntime::new(BoxliteOptions {
        home_dir: home.into(),
        ..Default::default()
    })
    .is_err();

    let session = GuestSession::new(BoxTransport::Unix {
        socket_path: box_dir.join("sockets/box.sock"),
    });
    let cmd = BoxCommand::new("echo")
        .args(["hello from the box"])
        .env(
            "BOXLITE_EXECUTOR",
            format!("container={}", container_id(&box_dir)?),
        )
        .working_dir("/");
    let token = CancellationToken::new();
    let mut exec = session.execution().await?.exec(cmd, token.clone()).await?;
    drop(exec.stdin_tx);

    let mut stdout = String::new();
    while let Some(chunk) = exec.stdout_rx.recv().await {
        stdout.push_str(&chunk);
    }
    let result = exec
        .result_rx
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("exec ended without a result"))?;
    token.cancel();
    Ok((lock_held, stdout, result.exit_code))
}

async fn clean_up(rt: &BoxliteRuntime, b: &LiteBox, home: &Path) {
    let _ = rt.remove(BOX_NAME, true).await;
    let _ = reap_box(home, b.id().as_ref());
}

/// The executables of the processes running a shim from under `home`.
fn shims(home: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let boxes = std::fs::canonicalize(home)?.join("boxes");
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Ok(exe) = std::fs::read_link(entry?.path().join("exe"))
            && exe.starts_with(&boxes)
            && exe.to_string_lossy().contains("boxlite-shim")
        {
            found.push(exe);
        }
    }
    Ok(found)
}

/// The container id is `pub(crate)` in BoxLite 0.11; it is the one directory name under
/// `boxes/<id>/shared/containers/`.
fn container_id(box_dir: &Path) -> anyhow::Result<String> {
    let mut ids = std::fs::read_dir(box_dir.join("shared/containers"))?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(ids.len() == 1, "expected one container, found {ids:?}");
    Ok(ids.pop().unwrap())
}
