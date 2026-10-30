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
use boxq::config::Egress;
use boxq::egress::Policy;
use boxq::reap::reap_box;
use boxq::sandbox::{ExecTarget, Listed, Sandboxes};
use boxq::spec::Spec;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Small and quick to pull; boxq's real image only matters to the Agent.
const IMAGE: &str = "alpine:3";

/// Alpine with git, for the tests that run git in a Sandbox.
const GIT_IMAGE: &str = "alpine/git:2.49.1";

/// `image` with BoxLite's default resources.
fn spec(image: &str) -> Spec {
    Spec {
        image: image.into(),
        cpus: None,
        memory_mib: None,
        disk_gib: None,
    }
}

/// A temporary boxq home with a temporary Workspace. Every Box in the home is removed on drop, so
/// a failing test never leaves a VM running.
struct Fixture {
    sandboxes: Sandboxes,
    workspace: Workspace,
    dirs: Vec<tempfile::TempDir>,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut f = Self {
            sandboxes: Sandboxes::new(home.path()),
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

/// The Egress Policy with the Global `allow` entries and the Workspace ones, applied only if
/// `approved`. Names are left unresolved, as no test host is on a tailnet.
fn policy(allow: &[&str], workspace_allow: &[&str], approved: bool) -> Policy {
    let egress = Egress {
        builtins: true,
        allow: allow.iter().map(|&e| e.into()).collect(),
        allow_host_loopback: false,
        workspace_allow: workspace_allow.iter().map(|&e| e.into()).collect(),
    };
    Policy::new(&egress, None, approved, |_| vec![]).unwrap()
}

/// The policy with no config files.
fn default_policy() -> Policy {
    policy(&[], &[], false)
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

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy())
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
        .up(&f.workspace, &spec(IMAGE), &default_policy())
        .await
        .unwrap();
    sh(&first.target, "echo kept > /root/state").await.unwrap();

    f.sandboxes.stop(&f.workspace).await.unwrap();
    let again = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy())
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
        .up(&f.workspace, &spec(IMAGE), &default_policy())
        .await
        .unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let after_stop = shims(&f.sandboxes.boxlite_home()).unwrap();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy())
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
        .up(&f.workspace, &spec(IMAGE), &default_policy())
        .await
        .unwrap();
    sh(&first.target, "echo gone > /root/state").await.unwrap();

    f.sandboxes.rm(&f.workspace).await.unwrap();
    let again = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy())
        .await
        .unwrap();

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
    f.sandboxes
        .up(&stopped, &spec(IMAGE), &default_policy())
        .await
        .unwrap();
    f.sandboxes.stop(&stopped).await.unwrap();
    f.sandboxes
        .up(&running, &spec(IMAGE), &default_policy())
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
        .up(&f.workspace, &spec(IMAGE), &default_policy())
        .await;
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

    let up = f
        .sandboxes
        .up(&worktree, &spec(GIT_IMAGE), &default_policy())
        .await
        .unwrap();
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
async fn a_new_sandbox_gets_the_configured_image_and_resources() {
    let f = Fixture::new();
    let spec = Spec {
        cpus: Some(2),
        memory_mib: Some(768),
        disk_gib: Some(3),
        ..spec(GIT_IMAGE)
    };

    let up = f
        .sandboxes
        .up(&f.workspace, &spec, &default_policy())
        .await
        .unwrap();
    let (out, code) = sh(
        &up.target,
        "nproc; grep MemTotal /proc/meminfo | tr -dc 0-9; echo; df -k / | awk 'NR==2{print $2}'; \
         git --version >/dev/null && echo has-git",
    )
    .await
    .unwrap();

    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let [cpus, mem_kib, disk_kib, git] = lines[..] else {
        panic!("unexpected output: {out:?}");
    };
    let mib = |kib: &str| kib.parse::<u64>().unwrap() / 1024;
    assert_eq!(cpus, "2");
    // The guest kernel keeps some memory for itself.
    assert!(
        (640..=768).contains(&mib(mem_kib)),
        "MemTotal {mem_kib} KiB"
    );
    assert!(
        (2560..=3072).contains(&mib(disk_kib)),
        "disk {disk_kib} KiB"
    );
    assert_eq!(git, "has-git");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_keeps_the_spec_it_was_created_with_until_removed() {
    let f = Fixture::new();
    let first = spec(IMAGE);
    let changed = Spec {
        cpus: Some(2),
        ..spec(IMAGE)
    };

    let created = f
        .sandboxes
        .up(&f.workspace, &first, &default_policy())
        .await
        .unwrap();
    let running = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy())
        .await
        .unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let restarted = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy())
        .await
        .unwrap();
    let (nproc, _) = sh(&restarted.target, "nproc").await.unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let recreated = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy())
        .await
        .unwrap();

    assert_eq!(created.as_created, Some(first.clone()));
    assert_eq!(running.as_created, Some(first.clone()));
    assert_eq!(restarted.as_created, Some(first));
    assert_eq!(nproc, "1\n");
    assert_eq!(recreated.as_created, Some(changed));
}

/// Whether an HTTPS GET of `url` from the Sandbox succeeds.
async fn reaches(target: &ExecTarget, url: &str) -> bool {
    let script = format!("wget -q -T 15 -O /dev/null {url}");
    sh(target, &script).await.unwrap().1 == 0
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM and internet access"]
async fn listed_hosts_are_reachable_and_unlisted_ones_refused() {
    let f = Fixture::new();
    let policy = policy(&["example.com"], &["example.org"], false);

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &policy)
        .await
        .unwrap();

    assert!(
        reaches(&up.target, "https://example.com").await,
        "a Global entry"
    );
    assert!(
        reaches(&up.target, "https://registry.npmjs.org/").await,
        "a built-in entry"
    );
    assert!(
        !reaches(&up.target, "https://example.net").await,
        "an unlisted host"
    );
    assert!(
        !reaches(&up.target, "https://example.org").await,
        "an unapproved Workspace entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM and internet access"]
async fn a_running_sandbox_gets_the_policy_of_each_up() {
    let f = Fixture::new();
    let first = f
        .sandboxes
        .up(
            &f.workspace,
            &spec(IMAGE),
            &policy(&["example.com"], &["example.org"], false),
        )
        .await
        .unwrap();
    let refused_before = !reaches(&first.target, "https://example.org").await;

    let again = f
        .sandboxes
        .up(
            &f.workspace,
            &spec(IMAGE),
            &policy(&[], &["example.org"], true),
        )
        .await
        .unwrap();

    assert!(refused_before);
    assert!(!again.created);
    assert!(
        reaches(&again.target, "https://example.org").await,
        "approved since"
    );
    assert!(
        !reaches(&again.target, "https://example.com").await,
        "dropped since"
    );
}
