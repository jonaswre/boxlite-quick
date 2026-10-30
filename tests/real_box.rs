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
use boxq::claude_config::ClaudeConfig;
use boxq::reap::reap_box;
use boxq::sandbox::{ExecTarget, Listed, Sandboxes};
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
    let up = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();
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
    let up = f.sandboxes.up(&f.workspace, IMAGE).await.unwrap();

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

/// Debian with Node, to install Claude Code into until the base image ships it.
const NODE_IMAGE: &str = "node:22-bookworm-slim";

/// The Claude Code release the Done-when test runs; prompts and their keys change between
/// releases, so this test is what proves the seeded keys still skip them.
const CLAUDE_CODE: &str = "@anthropic-ai/claude-code@2.1.312";

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
#[ignore = "boots a real Box; needs KVM"]
async fn claude_code_starts_with_the_hosts_skills_and_mcp_servers_and_no_prompts() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f.sandboxes.up(&f.workspace, NODE_IMAGE).await.unwrap();
    let install = format!("npm install -g --no-fund --no-audit {CLAUDE_CODE} >/dev/null 2>&1");
    assert_eq!(sh(&up.target, &install).await.unwrap().1, 0);

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
