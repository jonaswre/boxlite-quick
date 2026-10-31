//! Setup Hooks: `src/setup.rs`.

use super::*;
use boxq::sandbox::Up;
use boxq::setup::{self, Failed, Source};

fn hooks(global: Option<&str>, workspace: Option<&str>) -> SetupHooks {
    SetupHooks {
        global: global.map(Into::into),
        workspace: workspace.map(Into::into),
    }
}

impl Fixture {
    /// The Workspace's Sandbox from `image`, created or reattached as `boxq run` would.
    async fn up(&self, image: &str) -> Up {
        self.sandboxes
            .up(&self.workspace, &spec(image), &default_policy(), &self.auth)
            .await
            .unwrap()
    }

    /// Runs the Setup Hooks `up` hasn't finished, with no interrupt.
    async fn set_up(&self, up: &Up, hooks: &SetupHooks) -> Option<Failed> {
        setup::run_until(
            &self.sandboxes,
            &self.workspace,
            up,
            hooks,
            std::future::pending(),
        )
        .await
        .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_failed_setup_hook_runs_again_and_one_that_succeeded_does_not() {
    let f = Fixture::new();
    let global = "echo global >> /root/runs";
    let failing = hooks(Some(global), Some("echo failing >> /root/runs; exit 3"));
    let fixed = hooks(Some(global), Some("echo fixed >> /root/runs"));

    let created = f.up(IMAGE).await;
    let failed = f.set_up(&created, &failing).await;
    let retried = f.set_up(&f.up(IMAGE).await, &fixed).await;
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let restarted = f.up(IMAGE).await;
    let again = f.set_up(&restarted, &fixed).await;
    let (runs, _) = root_sh(&restarted.target, "cat /root/runs").await.unwrap();

    assert_eq!(
        failed,
        Some(Failed {
            source: Source::Workspace,
            status: 3,
            interrupted: false,
        })
    );
    assert_eq!(retried, None);
    assert_eq!(again, None);
    assert_eq!(runs, "global\nfailing\nfixed\n");
    let setup = restarted.setup.unwrap();
    assert!(setup.changes(&fixed).is_empty());
    assert_eq!(setup.changes(&failing), ["Workspace Setup Hook"]);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_setup_hook_runs_as_root_in_the_workspace_with_the_sessions_env_and_no_stdin() {
    let f = Fixture::new();
    let hook = r#"{ pwd; id -u; echo "$IS_SANDBOX"; cat; echo end; } > set-up"#;

    let up = f.up(NON_ROOT_IMAGE).await;
    let failed = f.set_up(&up, &hooks(None, Some(hook))).await;

    assert_eq!(failed, None);
    let out = std::fs::read_to_string(f.workspace.path().join("set-up")).unwrap();
    assert_eq!(
        out,
        format!("{}\n0\n1\nend\n", f.workspace.path().display())
    );
}

/// BoxLite signals only the process it started, so the hook's own children are the test.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn an_interrupted_setup_hook_is_killed_with_its_children_and_left_to_run_again() {
    let f = Fixture::new();
    let hooks = hooks(
        Some("sleep 1000 & sleep 1000"),
        Some("touch /root/workspace-ran"),
    );

    let up = f.up(IMAGE).await;
    let interrupt = async {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        libc::SIGINT
    };
    let failed = setup::run_until(&f.sandboxes, &f.workspace, &up, &hooks, interrupt)
        .await
        .unwrap();
    let (left, _) = root_sh(
        &up.target,
        // A killed child lingers as a zombie, listed as `[sleep]`, since the Box's init never
        // reaps it.
        "ps -o args | grep -c '^sleep 1000'; test -e /root/workspace-ran && echo ran",
    )
    .await
    .unwrap();
    let reattached = f.up(IMAGE).await;

    assert_eq!(
        failed,
        Some(Failed {
            source: Source::Global,
            status: 130,
            interrupted: true,
        })
    );
    // No sleep is left, and the Workspace Setup Hook never ran.
    assert_eq!(left, "0\n");
    let setup = reattached.setup.unwrap();
    assert!(!setup.global.done);
    assert!(!setup.workspace.done);
}

/// Sandboxes created before boxq kept a setup record must not run a hook they may never need.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_without_a_setup_record_counts_as_set_up() {
    let f = Fixture::new();
    f.up(IMAGE).await;
    let records = f.dirs[0].path().join("sandboxes");
    for entry in std::fs::read_dir(&records).unwrap() {
        let path = entry.unwrap().path();
        let record = std::fs::read_to_string(&path).unwrap();
        let without_setup = record.split("[setup").next().unwrap().to_string();
        std::fs::write(&path, without_setup).unwrap();
    }

    let up = f.up(IMAGE).await;
    let failed = f.set_up(&up, &hooks(Some("exit 1"), None)).await;

    assert_eq!(up.setup, None);
    assert_eq!(failed, None);
}
