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
use boxq::reap::reap_box;
use boxq::sandbox::{ExecTarget, Listed, Sandboxes};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Small and quick to pull; boxq's real image only matters to the Agent.
const IMAGE: &str = "alpine:3";

/// Alpine with git, for the tests that run git in a Sandbox.
const GIT_IMAGE: &str = "alpine/git:2.49.1";

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

    let up = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    let first = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
    sh(&first.target, "echo kept > /root/state").await.unwrap();

    f.sandboxes.stop(&f.workspace).await.unwrap();
    let again = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();

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

    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
    f.sandboxes.stop(&f.workspace).await.unwrap();
    let after_stop = shims(&f.sandboxes.boxlite_home()).unwrap();
    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    let first = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
    sh(&first.target, "echo gone > /root/state").await.unwrap();

    f.sandboxes.rm(&f.workspace).await.unwrap();
    let again = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();

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
    f.sandboxes.up(&stopped, IMAGE).await.unwrap();
    f.sandboxes.stop(&stopped).await.unwrap();
    f.sandboxes.up(&running, IMAGE).await.unwrap();
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

    let up = f.sandboxes.up(&f.workspace, IMAGE).await;
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

    let up = f.sandboxes.up(&worktree, GIT_IMAGE).await.unwrap();
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
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
    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    let up = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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

/// The interim image `boxq run` creates Sandboxes from, which has no Claude Code.
const INTERIM_IMAGE: &str = "node:22-bookworm";

/// Pulls a large image and Claude Code from npm, so it is slow and needs the network.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn claude_code_is_installed_once_in_a_sandbox_without_it() {
    let f = Fixture::new();
    let up = f.sandboxes.up(&f.workspace, INTERIM_IMAGE).await.unwrap();

    let first = boxq::claude::ensure_installed(&up.target).await.unwrap();
    let again = boxq::claude::ensure_installed(&up.target).await.unwrap();

    assert_eq!((first, again), (true, false));
    let (version, code) = sh(&up.target, "claude --version").await.unwrap();
    assert_eq!(code, 0, "claude --version printed {version:?}");
    assert!(version.contains("Claude Code"), "{version:?}");
}
