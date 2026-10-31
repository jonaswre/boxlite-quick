//! Creating, reattaching, stopping, removing and listing Sandboxes, and waiting on the BoxLite runtime: `src/sandbox.rs`, `src/runtime.rs` and `src/reap.rs`.

use super::*;
use boxlite::runtime::options::{BoxOptions, RootfsSpec};
use boxq::sandbox::Listed;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_command_runs_in_a_new_sandbox_while_another_process_holds_the_runtime() {
    let f = Fixture::new();

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let held = f.runtime();
    let ran = sh(&up.target, "echo hello from the sandbox").await.unwrap();
    drop(held);

    assert!(up.created);
    assert_eq!(ran, ("hello from the sandbox\n".into(), 0));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_stopped_sandbox_is_reattached_with_its_state() {
    let f = Fixture::new();
    let first = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    sh(&first.target, "echo kept > /root/state").await.unwrap();

    f.sandboxes.stop(&f.workspace).await.unwrap();
    let again = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    assert!(!again.created);
    let read = sh(&again.target, "cat /root/state").await.unwrap();
    assert_eq!(read, ("kept\n".into(), 0));
}

/// BoxLite 0.11 leaks the shim and VM on `stop` wherever it can't create the Box's cgroup,
/// which includes GitHub's runners (jonaswre/boxlite-quick#19). Remove `reap_box` once a BoxLite
/// release reaps the tree itself (boxlite-ai/boxlite#1240), and this test still passes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn no_shim_survives_stop_or_rm() {
    let f = Fixture::new();

    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let after_stop = shims(&f.sandboxes.boxlite_home()).unwrap();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let after_rm = shims(&f.sandboxes.boxlite_home()).unwrap();

    assert_eq!(after_stop, Vec::<PathBuf>::new(), "shims left after stop");
    assert_eq!(after_rm, Vec::<PathBuf>::new(), "shims left after rm");
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_removed_sandbox_is_created_afresh() {
    let f = Fixture::new();
    let first = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    sh(&first.target, "echo gone > /root/state").await.unwrap();

    f.sandboxes.rm(&f.workspace).await.unwrap();
    let again = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    assert!(again.created);
    let read = sh(&again.target, "test -e /root/state").await.unwrap();
    assert_eq!(read.1, 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "opens a real BoxLite runtime; needs BoxLite's host prerequisites"]
async fn stop_and_rm_refuse_a_workspace_without_a_sandbox() {
    let f = Fixture::new();

    let stop = f.sandboxes.stop(&f.workspace).await.unwrap_err();
    let rm = f.sandboxes.rm(&f.workspace).await.unwrap_err();

    let expected = format!("{} has no Sandbox", f.workspace.path().display());
    assert_eq!(
        (stop.to_string(), rm.to_string()),
        (expected.clone(), expected)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn sandboxes_are_listed_by_workspace_path() {
    let mut f = Fixture::new();
    let (a, b) = (f.another_workspace(), f.another_workspace());
    let (running, stopped) = if a.path() < b.path() { (a, b) } else { (b, a) };
    f.sandboxes
        .up(&stopped, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    f.sandboxes.stop(&stopped).await.unwrap();
    f.sandboxes
        .up(&running, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    // A Box boxq has no record of, as if its record were lost.
    let options = BoxOptions {
        rootfs: RootfsSpec::Image(IMAGE.into()),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    let rt = f.runtime();
    rt.create(options, Some("boxq-orphan".into()))
        .await
        .unwrap();
    drop(rt);

    let listed = f.sandboxes.list().await.unwrap();

    let row = |ws: Option<&Workspace>, name: &str, running| Listed {
        workspace: ws.map(|ws| ws.path().into()),
        box_name: name.into(),
        running,
    };
    assert_eq!(
        listed,
        [
            row(Some(&running), &running.box_name(), true),
            row(Some(&stopped), &stopped.box_name(), false),
            row(None, "boxq-orphan", false),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn up_waits_for_a_runtime_another_process_holds() {
    let f = Fixture::new();
    let held = f.runtime();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        drop(held);
    });

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await;
    release.join().unwrap();

    assert!(up.unwrap().created);
}
