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
use boxq::claude_config::ClaudeConfig;
use boxq::config::{BaseUrl, Egress, SetupHooks};
use boxq::egress::Policy;
use boxq::reap::reap_box;
use boxq::sandbox::{ExecTarget, Listed, Sandboxes, Up};
use boxq::setup::{self, Failed, Source};
use boxq::spec::{DEFAULT_CPUS, DEFAULT_DISK_GIB, DEFAULT_MEMORY_MIB, Spec};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

// Docker Hub images come through Google's mirror: every test pulls into a fresh home, and a run
// pulls more than Docker Hub's anonymous limit allows (jonaswre/boxlite-quick#43).

/// Small and quick to pull; boxq's real image only matters to the Agent.
const IMAGE: &str = "mirror.gcr.io/library/alpine:3";

/// Alpine with git, for the tests that run git in a Sandbox.
const GIT_IMAGE: &str = "mirror.gcr.io/alpine/git:2.49.1";

/// Alpine whose user is not root, for the tests that boxq runs as root whatever the image says.
const NON_ROOT_IMAGE: &str = "mirror.gcr.io/library/haproxy:3.2-alpine";

/// `image` with boxq's default resources.
fn spec(image: &str) -> Spec {
    Spec {
        image: image.into(),
        cpus: None,
        memory_mib: None,
        disk_gib: None,
    }
}

/// The image the Claude Code tests run in: boxq's Base Image, or `BOXQ_TEST_IMAGE`, which the
/// image workflow sets to the build it is about to publish.
fn claude_image() -> String {
    std::env::var("BOXQ_TEST_IMAGE").unwrap_or_else(|_| boxq::spec::BASE_IMAGE.into())
}

/// A key no test sends anywhere real, distinctive enough to grep a guest for.
const FAKE_KEY: &str = "boxq-fake-key-6f1c0e9a7b2d4e58";

/// Auth for tests that never reach the base URL.
fn fake_auth() -> Auth {
    Auth::new(
        &BaseUrl::parse("https://cliproxy.boxq.invalid").unwrap(),
        FAKE_KEY,
    )
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

/// The Egress Policy with the Global `allow` entries and the Workspace ones, applied only if
/// `approved`. Names are left unresolved, as no test host is on a tailnet.
fn policy(allow: &[&str], workspace_allow: &[&str], approved: bool) -> Policy {
    let egress = Egress {
        builtins: true,
        allow: allow.iter().map(|&e| e.into()).collect(),
        allow_host_loopback: false,
        workspace_allow: workspace_allow.iter().map(|&e| e.into()).collect(),
    };
    Policy::new(&egress, None, approved, |_| vec![])
}

/// The policy with no config files.
fn default_policy() -> Policy {
    policy(&[], &[], false)
}

/// Runs `sh -c script` through `target` as the image's user, returning its stdout and exit code.
async fn sh(target: &ExecTarget, script: &str) -> anyhow::Result<(String, i32)> {
    exec(target, target.command("sh").args(["-c", script])).await
}

/// [`sh`] as root.
async fn root_sh(target: &ExecTarget, script: &str) -> anyhow::Result<(String, i32)> {
    exec(
        target,
        target.command("sh").args(["-c", script]).user("root"),
    )
    .await
}

async fn exec(target: &ExecTarget, command: boxlite::BoxCommand) -> anyhow::Result<(String, i32)> {
    let token = CancellationToken::new();
    let mut exec = target
        .session()
        .execution()
        .await?
        .exec(command, token.clone())
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
        .up(&worktree, &spec(GIT_IMAGE), &default_policy(), &f.auth)
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
        .up(&f.workspace, &spec, &default_policy(), &f.auth)
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

/// The defaults fill in only the Box: the record keeps the Spec as configured, so a later
/// change to a default never reads as a config change (jonaswre/boxlite-quick#31).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_with_no_resources_configured_gets_boxqs_defaults() {
    let f = Fixture::new();

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let (out, code) = sh(
        &up.target,
        "nproc; grep MemTotal /proc/meminfo | tr -dc 0-9; echo; df -k / | awk 'NR==2{print $2}'",
    )
    .await
    .unwrap();

    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let [cpus, mem_kib, disk_kib] = lines[..] else {
        panic!("unexpected output: {out:?}");
    };
    let mib = |kib: &str| kib.parse::<u64>().unwrap() / 1024;
    assert_eq!(cpus, DEFAULT_CPUS.to_string());
    // The guest kernel keeps some memory for itself.
    let memory = u64::from(DEFAULT_MEMORY_MIB);
    assert!(
        (memory - 1024..=memory).contains(&mib(mem_kib)),
        "MemTotal {mem_kib} KiB"
    );
    assert!(
        ((DEFAULT_DISK_GIB - 2) * 1024..=DEFAULT_DISK_GIB * 1024).contains(&mib(disk_kib)),
        "disk {disk_kib} KiB"
    );
    assert_eq!(up.as_created, Some(spec(IMAGE)));
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
        .up(&f.workspace, &first, &default_policy(), &f.auth)
        .await
        .unwrap();
    let running = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy(), &f.auth)
        .await
        .unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let restarted = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy(), &f.auth)
        .await
        .unwrap();
    let (nproc, _) = sh(&restarted.target, "nproc").await.unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let recreated = f
        .sandboxes
        .up(&f.workspace, &changed, &default_policy(), &f.auth)
        .await
        .unwrap();

    assert_eq!(created.as_created, Some(first.clone()));
    assert_eq!(running.as_created, Some(first.clone()));
    assert_eq!(restarted.as_created, Some(first));
    assert_eq!(nproc, "4\n");
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
        .up(&f.workspace, &spec(IMAGE), &policy, &f.auth)
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
            &f.auth,
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
            &f.auth,
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_guest_holds_only_the_placeholder_never_the_key() {
    let f = Fixture::new();

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
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

/// BoxLite fixes a Box's secrets and env at creation, so a rotated key or a moved base URL
/// reaches only a recreated Sandbox.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_reports_when_it_was_created_with_other_auth() {
    let f = Fixture::new();
    let rotated = Auth::new(
        &BaseUrl::parse("https://cliproxy.boxq.invalid").unwrap(),
        "boxq-rotated-key",
    );
    let moved = Auth::new(
        &BaseUrl::parse("https://moved.boxq.invalid").unwrap(),
        FAKE_KEY,
    );
    let (spec, policy, same) = (spec(IMAGE), default_policy(), fake_auth());
    let up = |auth| f.sandboxes.up(&f.workspace, &spec, &policy, auth);

    let first = up(&f.auth).await.unwrap();
    let unchanged = up(&same).await.unwrap();
    let after_rotation = up(&rotated).await.unwrap();
    let after_move = up(&moved).await.unwrap();
    f.sandboxes.rm(&f.workspace).await.unwrap();
    let recreated = up(&rotated).await.unwrap();

    let changed = |up: &boxq::sandbox::Up| up.auth_changed;
    assert_eq!(
        [&first, &unchanged, &after_rotation, &after_move, &recreated].map(changed),
        [false, false, true, true, false]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_without_a_record_of_its_auth_reports_no_change() {
    let f = Fixture::new();
    // The Workspace's Box with no record, as one created before boxq recorded its auth.
    let options = BoxOptions {
        rootfs: RootfsSpec::Image(IMAGE.into()),
        entrypoint: Some(vec![]),
        cmd: Some(vec!["sleep".into(), "infinity".into()]),
        auto_delete: Some(0),
        detach: true,
        ..Default::default()
    };
    let rt = f.runtime();
    rt.create(options, Some(f.workspace.box_name()))
        .await
        .unwrap();
    drop(rt);

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    assert!(!up.created);
    assert!(!up.auth_changed);
}

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
    let base_url = BaseUrl::parse(&base_url).unwrap();
    let auth = Auth::new(&base_url, &key);
    let egress = Egress {
        builtins: true,
        allow: vec![],
        allow_host_loopback: false,
        workspace_allow: vec![],
    };
    // The base URL's real addresses, which a tailnet host needs next to its name.
    let resolve = |host: &str| {
        use std::net::{SocketAddr, ToSocketAddrs};
        let addrs = (host, 443).to_socket_addrs().unwrap();
        let v4 = addrs.filter_map(|a| match a {
            SocketAddr::V4(a) => Some(*a.ip()),
            SocketAddr::V6(_) => None,
        });
        v4.collect()
    };
    let policy = Policy::new(&egress, Some(&base_url), false, resolve);

    let up = f
        .sandboxes
        .up(&f.workspace, &spec(&claude_image()), &policy, &auth)
        .await
        .unwrap();
    let asked = sh(
        &up.target,
        "claude -p 'Reply with the single word pong, in lower case.' 2>&1",
    )
    .await
    .unwrap();

    assert_eq!(asked.1, 0, "claude failed: {}", asked.0);
    assert!(asked.0.to_lowercase().contains("pong"), "{}", asked.0);
}

/// A host Claude config with a symlinked skill, settings and MCP servers, as `boxq run` copies it.
fn host_claude_config(dir: &Path, ws: &Workspace) -> ClaudeConfig {
    let skill = dir.join("dotfiles/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "# Review\n").unwrap();
    std::fs::create_dir_all(dir.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink(&skill, dir.join(".claude/skills/review")).unwrap();
    std::fs::write(dir.join(".claude/.credentials.json"), "{}").unwrap();
    std::fs::write(
        dir.join(".claude/settings.json"),
        r#"{"model": "opus", "env": {"ANTHROPIC_API_KEY": "sk-secret"}}"#,
    )
    .unwrap();
    let claude_json = serde_json::json!({
        "mcpServers": {"docs": {"command": "docs-mcp"}},
        "oauthAccount": {"emailAddress": "me@example.com"},
        "projects": {ws.path().to_str().unwrap(): {"mcpServers": {"db": {"command": "db-mcp"}}}},
    });
    std::fs::write(dir.join(".claude.json"), claude_json.to_string()).unwrap();
    ClaudeConfig::new(&dir.join(".claude"), &dir.join(".claude.json"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_hosts_claude_config_merges_into_the_sandboxs_own() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    sh(
        &up.target,
        r#"mkdir -p /root/.claude && echo '{"numStartups": 3}' > /root/.claude.json &&
           echo '{"theme": "dark"}' > /root/.claude/settings.json"#,
    )
    .await
    .unwrap();

    host.sync(&f.workspace, &up.target).await.unwrap();

    let ws = f.workspace.path().to_str().unwrap();
    let read = |path: &'static str| {
        let target = &up.target;
        async move {
            let (out, code) = sh(target, &format!("cat {path}")).await.unwrap();
            assert_eq!(code, 0, "{path}");
            serde_json::from_str::<serde_json::Value>(&out).unwrap()
        }
    };
    assert_eq!(
        read("/root/.claude.json").await,
        serde_json::json!({
            "numStartups": 3,
            "hasCompletedOnboarding": true,
            "hasSeenAutoDefaultNudge": true,
            "mcpServers": {"docs": {"command": "docs-mcp"}},
            "projects": {ws: {
                "mcpServers": {"db": {"command": "db-mcp"}},
                "hasTrustDialogAccepted": true,
            }},
        })
    );
    assert_eq!(
        read("/root/.claude/settings.json").await,
        serde_json::json!({
            "theme": "dark",
            "model": "opus",
            "skipDangerousModePermissionPrompt": true,
            "permissions": {"defaultMode": "bypassPermissions"},
        })
    );
    let listed = sh(
        &up.target,
        "cat /root/.claude/skills/review/SKILL.md && ls -A /root/.claude && \
         stat -c %a /root/.claude.json",
    )
    .await
    .unwrap();
    assert_eq!(listed, ("# Review\nsettings.json\nskills\n600\n".into(), 0));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_sandbox_without_claude_config_gets_one() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    host.sync(&f.workspace, &up.target).await.unwrap();
    host.sync(&f.workspace, &up.target).await.unwrap();

    let ran = sh(
        &up.target,
        "grep -c hasTrustDialogAccepted /root/.claude.json && \
         cat /root/.claude/skills/review/SKILL.md",
    )
    .await
    .unwrap();
    assert_eq!(ran, ("1\n# Review\n".into(), 0));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_claude_config_larger_than_one_exec_message_is_synced() {
    let mut f = Fixture::new();
    let dir = f.temp_dir();
    let host = host_claude_config(&dir, &f.workspace);
    // BoxLite's guest refuses a single stdin message over 4 MiB.
    std::fs::write(dir.join("dotfiles/review/asset.bin"), vec![7u8; 5 << 20]).unwrap();
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        host.sync(&f.workspace, &up.target),
    )
    .await
    .expect("the sync hung")
    .unwrap();

    let synced = sh(&up.target, "wc -c < /root/.claude/skills/review/asset.bin")
        .await
        .unwrap();
    assert_eq!(synced, (format!("{}\n", 5 << 20), 0));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_claude_config_reaches_roots_home_whatever_the_images_user() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f
        .sandboxes
        .up(
            &f.workspace,
            &spec(NON_ROOT_IMAGE),
            &default_policy(),
            &f.auth,
        )
        .await
        .unwrap();
    let (user, _) = sh(&up.target, "id -u").await.unwrap();
    assert_ne!(user, "0\n", "the image's user is root");

    host.sync(&f.workspace, &up.target).await.unwrap();

    let synced = root_sh(&up.target, "cat /root/.claude/skills/review/SKILL.md")
        .await
        .unwrap();
    assert_eq!(synced, ("# Review\n".into(), 0));
}

/// What Claude Code's TUI draws in `target`'s Sandbox in its first seconds after `input` is
/// typed, escape codes and all.
async fn claude_screen(target: &ExecTarget, ws: &Workspace, input: &str) -> String {
    let token = CancellationToken::new();
    let claude = target
        .command("claude")
        .arg("--dangerously-skip-permissions")
        .env("IS_SANDBOX", "1")
        .env("TERM", "xterm-256color")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .env("ANTHROPIC_AUTH_TOKEN", "unused")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1")
        .working_dir(ws.path().to_str().unwrap())
        .tty(true);
    let mut exec = target
        .session()
        .execution()
        .await
        .unwrap()
        .exec(claude, token.clone())
        .await
        .unwrap();
    let stdin = exec.stdin_tx.take().unwrap();
    let mut screen = String::new();
    let mut typed = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, exec.stdout_rx.recv()).await {
        screen.push_str(&chunk);
        // Typed once the prompt is drawn, so the input lands in it.
        if !typed && screen.contains("bypass permissions on") {
            typed = true;
            stdin.send(input.as_bytes().to_vec()).unwrap();
        }
    }
    token.cancel();
    screen
}

/// `screen` without its terminal escape sequences, which split highlighted text.
fn plain(screen: &str) -> String {
    let mut out = String::new();
    let mut chars = screen.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {},
            Some(']') => while chars.next().is_some_and(|c| c != '\x07') {},
            _ => {}
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM and internet access"]
async fn claude_code_starts_with_the_hosts_skills_and_mcp_servers_and_no_prompts() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f
        .sandboxes
        .up(
            &f.workspace,
            &spec(&claude_image()),
            &default_policy(),
            &f.auth,
        )
        .await
        .unwrap();
    // Prompts and their keys change between releases, so this test is what proves the seeded
    // keys still skip them in the release the image ships.
    boxq::claude::check_installed(&up.target, &f.workspace)
        .await
        .unwrap();

    host.sync(&f.workspace, &up.target).await.unwrap();

    let ws = f.workspace.path().display();
    let (mcp, _) = sh(&up.target, &format!("cd {ws} && claude mcp list 2>&1"))
        .await
        .unwrap();
    assert!(
        mcp.contains("docs: docs-mcp") && mcp.contains("db: db-mcp"),
        "{mcp}"
    );
    let screen = claude_screen(&up.target, &f.workspace, "/revi").await;
    assert!(screen.contains("bypass permissions on"), "{screen}");
    // The synced skill, offered as a user-scope command.
    assert!(plain(&screen).contains("Review (user)"), "{screen}");
    // The onboarding theme picker, the trust dialog, the bypass warning and the auto-mode offer.
    for prompt in ["text style", "trust", "I accept", "auto mode"] {
        assert!(
            !screen.contains(prompt),
            "prompted with {prompt:?}: {screen}"
        );
    }
}

/// A `boxq` process on its own pseudo-terminal, as a user's terminal runs it.
struct Terminal {
    master: std::fs::File,
    child: std::process::Child,
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    seen: String,
}

impl Terminal {
    /// Runs `boxq args` against the fixture's home on a fresh 24x80 terminal.
    fn boxq(f: &Fixture, args: &[&str]) -> Self {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use std::os::unix::process::CommandExt;

        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(std::ffi::OsStr::from_bytes(
                ptsname(&master, Vec::new()).unwrap().as_bytes(),
            ))
            .unwrap();
        let master = std::fs::File::from(master);
        set_size(&master, 24, 80);
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_boxq"));
        cmd.args(args)
            .env("BOXQ_HOME", f.sandboxes.boxlite_home().parent().unwrap())
            .env("XDG_CONFIG_HOME", f.sandboxes.boxlite_home())
            .env("XDG_STATE_HOME", f.sandboxes.boxlite_home())
            .env("BOXQ_BASE_URL", "https://cliproxy.boxq.invalid")
            .env("BOXQ_API_KEY", FAKE_KEY)
            .env("TERM", "xterm-256color")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        // The terminal becomes boxq's controlling terminal, so resizing it sends SIGWINCH.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        // The slave stays open only in boxq, so the reader sees EOF when boxq exits.
        drop(cmd);
        let (tx, output) = std::sync::mpsc::channel();
        let mut reader = master.try_clone().unwrap();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            master,
            child,
            output,
            seen: String::new(),
        }
    }

    fn type_in(&mut self, keys: &str) {
        use std::io::Write;
        self.master.write_all(keys.as_bytes()).unwrap();
    }

    fn resize(&self, rows: u16, cols: u16) {
        set_size(&self.master, rows, cols);
    }

    /// Waits for `text` in the terminal's output, panicking with what it saw after 60 s.
    fn expect(&mut self, text: &str) {
        self.expect_within(text, std::time::Duration::from_secs(60));
    }

    /// Waits for `text` in the terminal's output, panicking with what it saw after `wait`.
    fn expect_within(&mut self, text: &str, wait: std::time::Duration) {
        let deadline = std::time::Instant::now() + wait;
        while !self.seen.contains(text) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => panic!("no {text:?} on the terminal; it shows:\n{}", self.seen),
            }
        }
        // Later expectations only look at what comes after.
        let at = self.seen.find(text).unwrap() + text.len();
        self.seen.drain(..at);
    }

    /// Waits for a login shell's prompt in `ws`: Alpine's `/etc/profile` sets it to `host:cwd# `.
    fn expect_prompt(&mut self, ws: &Workspace) {
        self.expect(&format!(":{}# ", ws.path().display()));
    }

    /// Waits for boxq to exit, returning its exit code.
    fn exit_code(&mut self) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status
                    .code()
                    .unwrap_or_else(|| panic!("boxq died: {status}"));
            }
            assert!(
                std::time::Instant::now() < deadline,
                "boxq did not exit; the terminal shows:\n{}",
                self.seen
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn set_size(master: &std::fs::File, rows: u16, cols: u16) {
    let size = rustix::termios::Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    rustix::termios::tcsetwinsize(master, size).unwrap();
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn shell_opens_a_root_shell_in_the_workspace_and_leaves_the_sandbox_running() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let ws = f.workspace.path().to_str().unwrap();

    let mut t = Terminal::boxq(&f, &["shell", ws]);
    t.expect_prompt(&f.workspace);
    // Typed `$` is echoed as-is, so only the shell's own output matches.
    t.type_in("echo \"at:$PWD:uid=$(id -u):$IS_SANDBOX:$TERM\"\r");
    t.expect(&format!("at:{ws}:uid=0:1:xterm-256color"));
    t.type_in("exit 3\r");

    assert_eq!(t.exit_code(), 3);
    let listed = f.sandboxes.list().await.unwrap();
    assert!(listed[0].running, "the Sandbox stopped with the session");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_session_follows_its_terminals_size() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    t.type_in("echo \"size:$(stty size)\"\r");
    t.expect("size:24 80");

    t.resize(40, 100);
    // The resize and the typed command reach the guest on separate channels, so wait first.
    std::thread::sleep(std::time::Duration::from_secs(1));
    t.type_in("echo \"size:$(stty size)\"\r");

    t.expect("size:40 100");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn ctrl_c_interrupts_the_guests_command_not_boxq() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    t.type_in("sleep 600\r");
    std::thread::sleep(std::time::Duration::from_secs(1));

    t.type_in("\x03");

    // The prompt is back long before the sleep ends, and a shell reports SIGINT as 130.
    t.expect_prompt(&f.workspace);
    t.type_in("echo \"interrupted:$?\"\r");
    t.expect("interrupted:130");
    t.type_in("exit\r");
    assert_eq!(t.exit_code(), 0);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn a_hangup_sent_to_boxq_reaches_the_session() {
    let f = Fixture::new();
    f.sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    let mut t = Terminal::boxq(&f, &["shell", f.workspace.path().to_str().unwrap()]);
    t.expect_prompt(&f.workspace);
    rustix::process::kill_process(
        rustix::process::Pid::from_child(&t.child),
        rustix::process::Signal::HUP,
    )
    .unwrap();

    // The guest's shell dies of SIGHUP, which a shell reports as 129.
    assert_eq!(t.exit_code(), 129);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn run_starts_claude_in_the_workspace_and_shell_works_beside_it() {
    let f = Fixture::new();
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();
    // A stand-in for Claude Code that shows how it was started, then stays interactive.
    let stub = r#"printf '#!/bin/sh\necho "claude:$*:$PWD:$IS_SANDBOX"\nexec sh -l\n' \
        > /usr/local/bin/claude && chmod +x /usr/local/bin/claude"#;
    assert_eq!(sh(&up.target, stub).await.unwrap().1, 0);
    let ws = f.workspace.path().to_str().unwrap();

    let mut run = Terminal::boxq(&f, &["run", ws]);
    run.expect(&format!("claude:--dangerously-skip-permissions:{ws}:1"));
    run.expect_prompt(&f.workspace);
    let mut shell = Terminal::boxq(&f, &["shell", ws]);
    shell.expect_prompt(&f.workspace);
    shell.type_in("echo beside > /root/note; exit\r");
    let shell_exit = shell.exit_code();
    run.type_in("echo \"from:$(cat /root/note)\"\r");
    run.expect("from:beside");
    run.type_in("exit 4\r");

    assert_eq!((shell_exit, run.exit_code()), (0, 4));
}

/// Pulls boxq's Base Image, so it is slow and needs the network.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM and internet access"]
async fn run_on_a_fresh_workspace_starts_claude_code_from_the_pinned_base_image() {
    let f = Fixture::new();
    let ws = f.workspace.path().to_str().unwrap();

    let mut run = Terminal::boxq(&f, &["run", ws]);
    run.resize(40, 160);
    // The run pulls the Base Image into the fixture's empty home first.
    run.expect_within("bypass permissions on", std::time::Duration::from_secs(600));
    drop(run);
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    assert_eq!(up.as_created.unwrap().image, boxq::spec::BASE_IMAGE);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn an_image_without_claude_code_is_refused_not_installed_into() {
    let f = Fixture::new();
    let up = f
        .sandboxes
        .up(&f.workspace, &spec(IMAGE), &default_policy(), &f.auth)
        .await
        .unwrap();

    let checked = boxq::claude::check_installed(&up.target, &f.workspace).await;

    let error = checked.unwrap_err().to_string();
    assert!(
        error.contains("no `claude`") && error.contains("boxq rm"),
        "{error}"
    );
}

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
