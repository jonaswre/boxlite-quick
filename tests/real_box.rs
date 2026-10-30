//! Boots a real Box, so it needs Linux x86_64, KVM and BoxLite's host prerequisites. Run with
//! `cargo test --test real_box -- --ignored`; CI runs it on every push.
//!
//! boxq execs through the Box's own `box.sock` so no runtime (and no `BOXLITE_HOME` lock) is
//! held during a session (jonaswre/boxlite-quick#8). That leans on BoxLite internals: the
//! socket layout, the `BOXLITE_EXECUTOR` env protocol and the container-id directory. This
//! test pins them, so any BoxLite bump must pass it. Once boxq has its own lock-free exec
//! target (jonaswre/boxlite-quick#10), this test should drive that instead of BoxLite directly.

use boxlite::runtime::options::{BoxOptions, BoxliteOptions, RootfsSpec};
use boxlite::{BoxCommand, BoxliteRuntime, GuestSession};
use boxlite_shared::BoxTransport;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const BOX_NAME: &str = "boxq-real-box-test";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_command_runs_through_box_sock_while_the_runtime_is_held() {
    let home = tempfile::tempdir().unwrap();
    let rt = BoxliteRuntime::new(BoxliteOptions {
        home_dir: home.path().into(),
        ..Default::default()
    })
    .unwrap();

    let ran = run_while_held(&rt, home.path()).await;

    // Clean up before asserting, so a failure never leaves a VM running.
    if let Ok(Some(b)) = rt.get(BOX_NAME).await {
        let _ = b.stop().await;
    }
    let _ = rt.remove(BOX_NAME, true).await;
    kill_leftovers(home.path());

    let (lock_held, stdout, exit_code) = ran.unwrap();
    assert!(lock_held, "the runtime should hold the BOXLITE_HOME lock");
    assert_eq!((stdout.as_str(), exit_code), ("hello from the box\n", 0));
}

/// Starts the Box, then, with `rt` still open, runs `echo` in it through `box.sock`.
async fn run_while_held(rt: &BoxliteRuntime, home: &Path) -> anyhow::Result<(bool, String, i32)> {
    let options = BoxOptions {
        rootfs: RootfsSpec::Image("alpine:3".into()),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    let b = rt.create(options, Some(BOX_NAME.into())).await?;
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

/// BoxLite 0.11 leaks the Box's shim and VM on `stop` wherever it can't create a cgroup
/// (jonaswre/boxlite-quick#19). Kill whatever still runs from this test's home; boxq's own
/// reaper (jonaswre/boxlite-quick#21) replaces this and asserts that nothing survives.
fn kill_leftovers(home: &Path) {
    let _ = std::process::Command::new("pkill")
        .arg("-KILL")
        .arg("-f")
        .arg(home)
        .status();
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
