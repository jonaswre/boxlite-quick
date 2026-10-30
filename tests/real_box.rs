//! Boots real Boxes, so it needs Linux x86_64, KVM and BoxLite's host prerequisites. Run with
//! `cargo test --test real_box -- --ignored`; CI runs it on every push.
//!
//! boxq execs through the Box's own `box.sock` so no runtime (and no `BOXLITE_HOME` lock) is
//! held during a session (jonaswre/boxlite-quick#8). That leans on BoxLite internals: the
//! socket layout, the `BOXLITE_EXECUTOR` env protocol and the container-id directory. These
//! tests drive boxq's own exec target, so any BoxLite bump must pass them.

use boxlite::BoxliteRuntime;
use boxlite::runtime::options::{BoxOptions, BoxliteOptions, RootfsSpec};
use boxq::Workspace;
use boxq::auth::Auth;
use boxq::reap::reap_box;
use boxq::sandbox::{ExecTarget, Listed, Sandboxes};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Small and quick to pull; boxq's real image only matters to the Agent.
const IMAGE: &str = "alpine:3";

/// Alpine with git, for the tests that run git in a Sandbox.
const GIT_IMAGE: &str = "alpine/git:2.49.1";

/// Node, for the test that installs Claude Code from npm.
const NODE_IMAGE: &str = "node:22-bookworm";

/// A key no test sends anywhere real, distinctive enough to grep a guest for.
const FAKE_KEY: &str = "boxq-fake-key-6f1c0e9a7b2d4e58";

/// Auth for tests that never reach the base URL.
fn fake_auth() -> Auth {
    Auth::new("https://cliproxy.boxq.invalid", FAKE_KEY).unwrap()
}

/// A temporary boxq home with a temporary Workspace. Every Box in the home is removed on drop, so
/// a failing test never leaves a VM running.
struct Fixture {
    sandboxes: Sandboxes,
    auth: Auth,
    workspace: Workspace,
    dirs: Vec<tempfile::TempDir>,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut f = Self {
            sandboxes: Sandboxes::new(home.path()),
            auth: fake_auth(),
            workspace: Workspace::resolve(home.path()).unwrap(),
            dirs: vec![home],
        };
        f.workspace = f.another_workspace();
        f
    }

    fn another_workspace(&mut self) -> Workspace {
        Workspace::resolve(&self.temp_dir()).unwrap()
    }

    /// A directory removed with the fixture.
    fn temp_dir(&mut self) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().into();
        self.dirs.push(dir);
        path
    }

    /// A BoxLite runtime on the home, as another process would open it.
    fn runtime(&self) -> BoxliteRuntime {
        BoxliteRuntime::new(BoxliteOptions {
            home_dir: self.sandboxes.boxlite_home(),
            ..Default::default()
        })
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let home = self.sandboxes.boxlite_home();
        let rt = self.runtime();
        let clean_up = async {
            for b in rt.list_info().await.unwrap_or_default() {
                let _ = rt.remove(b.id.as_ref(), true).await;
                let _ = reap_box(&home, b.id.as_ref());
            }
        };
        // Drop runs inside the test's own tokio runtime, which can't be blocked on.
        std::thread::scope(|s| {
            s.spawn(|| tokio::runtime::Runtime::new().unwrap().block_on(clean_up));
        });
    }
}

/// Runs `sh -c script` through `target`, returning its stdout and exit code.
async fn sh(target: &ExecTarget, script: &str) -> anyhow::Result<(String, i32)> {
    let token = CancellationToken::new();
    let mut exec = target
        .session()
        .execution()
        .await?
        .exec(target.command("sh").args(["-c", script]), token.clone())
        .await?;
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
    Ok((stdout, result.exit_code))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_command_runs_in_a_new_sandbox_while_another_process_holds_the_runtime() {
    let f = Fixture::new();

    let up = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
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
    let first = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
    sh(&first.target, "echo kept > /root/state").await.unwrap();

    f.sandboxes.stop(&f.workspace).await.unwrap();
    let again = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();

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

    f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let after_stop = shims(&f.sandboxes.boxlite_home()).unwrap();
    f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
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
    let first = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
    sh(&first.target, "echo gone > /root/state").await.unwrap();

    f.sandboxes.rm(&f.workspace).await.unwrap();
    let again = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();

    assert!(again.created);
    let read = sh(&again.target, "test -e /root/state").await.unwrap();
    assert_eq!(read.1, 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
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
    f.sandboxes.up(&stopped, IMAGE, &f.auth).await.unwrap();
    f.sandboxes.stop(&stopped).await.unwrap();
    f.sandboxes.up(&running, IMAGE, &f.auth).await.unwrap();
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

    let up = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await;
    release.join().unwrap();

    assert!(up.unwrap().created);
}

/// Runs host git in `dir`, returning its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=boxq", "-c", "user.email=boxq@example.com"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn git_works_in_a_worktrees_sandbox_as_on_the_host() {
    let mut f = Fixture::new();
    let tmp = f.temp_dir();
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    let worktree = Workspace::resolve(&tmp.join("feature")).unwrap();

    let up = f.sandboxes.up(&worktree, GIT_IMAGE, &f.auth).await.unwrap();
    let script = format!(
        "cd {} && echo hi > note && git add note && \
         git -c user.name=boxq -c user.email=boxq@example.com commit -qm 'from the Sandbox' && \
         git status --short --branch",
        worktree.path().display()
    );
    let ran = sh(&up.target, &script).await.unwrap();

    assert_eq!(ran, ("## feature\n".into(), 0));
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s", "feature"]),
        "from the Sandbox\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("note")).unwrap(),
        "hi\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_guest_holds_only_the_placeholder_never_the_key() {
    let f = Fixture::new();

    let up = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
    let token = sh(&up.target, "printf %s \"$ANTHROPIC_AUTH_TOKEN\"")
        .await
        .unwrap();
    // Every process's env and command line, and every file on the guest's own disk, read out
    // to be searched here: searching in the guest would need the key in the guest.
    let dump = sh(
        &up.target,
        "for p in /proc/[0-9]*; do cat $p/environ $p/cmdline; echo; done; cat /proc/cmdline; \
         find / -xdev -type f -exec cat {} + ; true",
    )
    .await
    .unwrap();

    assert_eq!(token, ("<BOXLITE_SECRET:anthropic>".into(), 0));
    assert!(
        dump.0.contains("<BOXLITE_SECRET:anthropic>"),
        "dump missed the env"
    );
    assert!(!dump.0.contains(FAKE_KEY), "the key is in the guest");
}

/// The Claude Code release the auth test installs, as the image will pin one.
const CLAUDE_CODE: &str = "@anthropic-ai/claude-code@2.1.312";

/// Needs a real base URL and key, so it reads them from `BOXQ_TEST_BASE_URL` and
/// `BOXQ_TEST_API_KEY` and passes vacuously without them, as in CI.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn claude_code_authenticates_through_the_placeholder() {
    let (Ok(base_url), Ok(key)) = (
        std::env::var("BOXQ_TEST_BASE_URL"),
        std::env::var("BOXQ_TEST_API_KEY"),
    ) else {
        eprintln!("skipped: set BOXQ_TEST_BASE_URL and BOXQ_TEST_API_KEY to run it");
        return;
    };
    let f = Fixture::new();
    let auth = Auth::new(&base_url, &key).unwrap();

    let up = f
        .sandboxes
        .up(&f.workspace, NODE_IMAGE, &auth)
        .await
        .unwrap();
    let install = sh(
        &up.target,
        &format!("npm install -g --no-fund --no-audit {CLAUDE_CODE} >&2"),
    )
    .await
    .unwrap();
    let asked = sh(
        &up.target,
        "claude -p 'Reply with the single word pong, in lower case.' 2>&1",
    )
    .await
    .unwrap();

    assert_eq!(install.1, 0, "npm install failed");
    assert_eq!(asked.1, 0, "claude failed: {}", asked.0);
    assert!(asked.0.to_lowercase().contains("pong"), "{}", asked.0);
}

/// BoxLite fixes a Box's secrets and env at creation, so a rotated key or a moved base URL
/// reaches only a recreated Sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_reports_when_it_was_created_with_other_auth() {
    let f = Fixture::new();
    let rotated = Auth::new("https://cliproxy.boxq.invalid", "boxq-rotated-key").unwrap();
    let moved = Auth::new("https://moved.boxq.invalid", FAKE_KEY).unwrap();

    let first = f.sandboxes.up(&f.workspace, IMAGE, &f.auth).await.unwrap();
    let same = f
        .sandboxes
        .up(&f.workspace, IMAGE, &fake_auth())
        .await
        .unwrap();
    let after_rotation = f.sandboxes.up(&f.workspace, IMAGE, &rotated).await.unwrap();
    let after_move = f.sandboxes.up(&f.workspace, IMAGE, &moved).await.unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let recreated = f.sandboxes.up(&f.workspace, IMAGE, &rotated).await.unwrap();

    let changed = |up: &boxq::sandbox::Up| up.auth_changed;
    assert_eq!(
        [&first, &same, &after_rotation, &after_move, &recreated].map(changed),
        [false, false, true, true, false]
    );
}
