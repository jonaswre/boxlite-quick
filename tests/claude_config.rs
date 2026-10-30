use boxq::Workspace;
use boxq::claude_config::{ClaudeConfig, GuestFile};
use std::path::{Path, PathBuf};

/// A fake host: its Claude config dir and `.claude.json`, and a Workspace.
struct Host {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    workspace: Workspace,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("home/.claude")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        Self {
            workspace: Workspace::resolve(&root.join("project")).unwrap(),
            _tmp: tmp,
            root,
        }
    }

    fn claude_dir(&self) -> PathBuf {
        self.root.join("home/.claude")
    }

    /// Writes `contents` at `rel` under the host's Claude config dir.
    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.claude_dir().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn config(&self) -> ClaudeConfig {
        ClaudeConfig::new(&self.claude_dir(), &self.root.join("home/.claude.json"))
    }

    /// The files a sync writes into a fresh Sandbox's home.
    fn fresh(&self) -> Vec<GuestFile> {
        self.config()
            .guest_files(&self.workspace, None, None)
            .unwrap()
    }
}

fn find<'a>(files: &'a [GuestFile], path: &str) -> Option<&'a GuestFile> {
    files.iter().find(|f| f.path == Path::new(path))
}

fn text(files: &[GuestFile], path: &str) -> String {
    let file = find(files, path).unwrap_or_else(|| panic!("{path} not synced: {files:?}"));
    String::from_utf8(file.contents.clone()).unwrap()
}

#[test]
fn a_symlinked_skill_is_copied_as_the_files_it_points_at() {
    let host = Host::new();
    let real = host.root.join("dotfiles/review");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("SKILL.md"), "# Review\n").unwrap();
    std::fs::create_dir_all(host.claude_dir().join("skills")).unwrap();
    std::os::unix::fs::symlink(&real, host.claude_dir().join("skills/review")).unwrap();

    let files = host.fresh();

    assert_eq!(text(&files, ".claude/skills/review/SKILL.md"), "# Review\n");
}

#[test]
fn the_copy_list_is_synced_into_the_guests_claude_dir() {
    let host = Host::new();
    let copied = [
        "CLAUDE.md",
        "rules/style.md",
        "skills/tdd/SKILL.md",
        "agents/reviewer.md",
        "commands/ship.md",
        "output-styles/terse.md",
    ];
    for rel in copied {
        host.write(rel, rel);
    }

    let files = host.fresh();

    for rel in copied {
        assert_eq!(text(&files, &format!(".claude/{rel}")), rel);
    }
}

#[test]
fn credentials_and_host_state_are_never_synced() {
    let host = Host::new();
    for rel in [
        ".credentials.json",
        "backups/.claude.json.backup",
        "projects/x/session.jsonl",
        "history.jsonl",
        "plugins/installed_plugins.json",
        "remote-settings.json",
        "skills/synced/SKILL.md",
        "skills/.trash/old/SKILL.md",
    ] {
        host.write(rel, "secret");
    }

    let leaked: Vec<_> = host
        .fresh()
        .into_iter()
        .filter(|f| f.contents == b"secret")
        .map(|f| f.path)
        .collect();

    assert_eq!(leaked, Vec::<PathBuf>::new());
}

#[test]
fn a_symlink_loop_is_copied_once() {
    let host = Host::new();
    host.write("skills/tdd/SKILL.md", "# TDD\n");
    std::os::unix::fs::symlink("..", host.claude_dir().join("skills/tdd/up")).unwrap();

    let files = host.fresh();

    let skills: Vec<_> = files
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| p.starts_with(".claude/skills"))
        .collect();
    assert_eq!(skills, [PathBuf::from(".claude/skills/tdd/SKILL.md")]);
}

fn json(files: &[GuestFile], path: &str) -> serde_json::Value {
    serde_json::from_str(&text(files, path)).unwrap()
}

#[test]
fn host_settings_are_synced_without_credentials_or_host_paths() {
    let host = Host::new();
    host.write(
        "settings.json",
        r#"{
            "model": "opus",
            "env": {"ANTHROPIC_API_KEY": "sk-secret"},
            "apiKeyHelper": "/home/me/bin/key",
            "awsCredentialExport": "/home/me/bin/aws",
            "awsAuthRefresh": "aws sso login",
            "gcpAuthRefresh": "gcloud auth login",
            "otelHeadersHelper": "/home/me/bin/otel",
            "hooks": {"Stop": []},
            "statusLine": {"type": "command", "command": "/home/me/bin/status"},
            "enabledPlugins": {"x@y": true},
            "extraKnownMarketplaces": {"y": {}}
        }"#,
    );

    let settings = json(&host.fresh(), ".claude/settings.json");

    let mut keys: Vec<_> = settings.as_object().unwrap().keys().cloned().collect();
    keys.retain(|k| !["skipDangerousModePermissionPrompt", "permissions"].contains(&&**k));
    assert_eq!(keys, ["model"]);
    assert_eq!(settings["model"], "opus");
}

#[test]
fn host_settings_merge_into_the_guests_with_the_bypass_prompt_skipped() {
    let host = Host::new();
    host.write(
        "settings.json",
        r#"{"model": "opus", "permissions": {"allow": ["Bash(git:*)"], "defaultMode": "plan"}}"#,
    );
    let guest = r#"{"theme": "dark", "model": "sonnet", "permissions": {"allow": ["Bash(ls)"]}}"#;

    let files = host
        .config()
        .guest_files(&host.workspace, None, Some(guest))
        .unwrap();

    assert_eq!(
        json(&files, ".claude/settings.json"),
        serde_json::json!({
            "theme": "dark",
            "model": "opus",
            "permissions": {"allow": ["Bash(git:*)"], "defaultMode": "bypassPermissions"},
            "skipDangerousModePermissionPrompt": true,
        })
    );
}

impl Host {
    fn write_claude_json(&self, json: serde_json::Value) {
        std::fs::write(self.root.join("home/.claude.json"), json.to_string()).unwrap();
    }
}

#[test]
fn a_fresh_sandbox_gets_the_hosts_mcp_servers_and_skips_its_first_run_prompts() {
    let host = Host::new();
    let ws = host.workspace.path().to_str().unwrap();
    host.write_claude_json(serde_json::json!({
        "mcpServers": {"docs": {"command": "npx", "args": ["docs-mcp"]}},
        "oauthAccount": {"emailAddress": "me@example.com"},
        "userID": "abc",
        "primaryApiKey": "sk-secret",
        "projects": {"/elsewhere": {"hasTrustDialogAccepted": true}},
    }));

    let claude_json = json(&host.fresh(), ".claude.json");

    assert_eq!(
        claude_json,
        serde_json::json!({
            "hasCompletedOnboarding": true,
            "hasSeenAutoDefaultNudge": true,
            "mcpServers": {"docs": {"command": "npx", "args": ["docs-mcp"]}},
            "projects": {ws: {"hasTrustDialogAccepted": true}},
        })
    );
}

#[test]
fn the_workspaces_local_mcp_servers_merge_into_the_guests_own_state() {
    let host = Host::new();
    let ws = host.workspace.path().to_str().unwrap();
    host.write_claude_json(serde_json::json!({"projects": {ws: {
        "mcpServers": {"db": {"command": "db-mcp"}},
        "enabledMcpjsonServers": ["lint"],
        "disabledMcpjsonServers": ["slow"],
        "allowedTools": ["Bash"],
        "lastCost": 1.5,
    }}}));
    let guest = serde_json::json!({
        "numStartups": 7,
        "projects": {ws: {"lastSessionId": "s1", "mcpServers": {"old": {}}}},
    });

    let files = host
        .config()
        .guest_files(&host.workspace, Some(&guest.to_string()), None)
        .unwrap();

    assert_eq!(
        json(&files, ".claude.json"),
        serde_json::json!({
            "numStartups": 7,
            "hasCompletedOnboarding": true,
            "hasSeenAutoDefaultNudge": true,
            "projects": {ws: {
                "lastSessionId": "s1",
                "mcpServers": {"db": {"command": "db-mcp"}},
                "enabledMcpjsonServers": ["lint"],
                "disabledMcpjsonServers": ["slow"],
                "hasTrustDialogAccepted": true,
            }},
        })
    );
}

/// Runs host git in `dir`, failing the test if git does.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=boxq", "-c", "user.email=boxq@example.com"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Claude Code keys trust on the main checkout's root for a worktree.
#[test]
fn a_worktree_trusts_its_main_checkouts_root_too() {
    let host = Host::new();
    let repo = host.root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&repo, &["worktree", "add", "-q", "../feature"]);
    let worktree = Workspace::resolve(&host.root.join("feature")).unwrap();
    let main = std::fs::canonicalize(&repo).unwrap();
    let main = main.to_str().unwrap();
    host.write_claude_json(serde_json::json!({"projects": {main: {
        "mcpServers": {"db": {"command": "db-mcp"}},
    }}}));

    let files = host.config().guest_files(&worktree, None, None).unwrap();

    assert_eq!(
        json(&files, ".claude.json")["projects"],
        serde_json::json!({
            worktree.path().to_str().unwrap(): {"hasTrustDialogAccepted": true},
            main: {"mcpServers": {"db": {"command": "db-mcp"}}, "hasTrustDialogAccepted": true},
        })
    );
}

#[test]
fn malformed_host_settings_are_an_error_naming_the_file() {
    let host = Host::new();
    let path = host.write("settings.json", "{not json");

    let err = host
        .config()
        .guest_files(&host.workspace, None, None)
        .unwrap_err();

    assert!(err.to_string().contains(&*path.to_string_lossy()), "{err}");
}

#[test]
fn the_hosts_claude_config_is_found_under_home() {
    let host = Host::new();
    host.write("CLAUDE.md", "home");
    host.write_claude_json(serde_json::json!({"mcpServers": {"home": {}}}));
    let home = host.root.join("home").to_string_lossy().into_owned();

    let config = ClaudeConfig::from_env(|key| (key == "HOME").then(|| home.clone())).unwrap();
    let files = config.guest_files(&host.workspace, None, None).unwrap();

    assert_eq!(text(&files, ".claude/CLAUDE.md"), "home");
    assert_eq!(
        json(&files, ".claude.json")["mcpServers"],
        serde_json::json!({"home": {}})
    );
}

/// `CLAUDE_CONFIG_DIR` moves the whole config, `.claude.json` included.
#[test]
fn claude_config_dir_moves_the_hosts_claude_config() {
    let host = Host::new();
    let dir = host.root.join("elsewhere");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("CLAUDE.md"), "moved").unwrap();
    std::fs::write(dir.join(".claude.json"), r#"{"mcpServers": {"moved": {}}}"#).unwrap();
    let env = |key: &str| match key {
        "HOME" => Some(host.root.join("home").to_string_lossy().into_owned()),
        "CLAUDE_CONFIG_DIR" => Some(dir.to_string_lossy().into_owned()),
        _ => None,
    };

    let config = ClaudeConfig::from_env(env).unwrap();
    let files = config.guest_files(&host.workspace, None, None).unwrap();

    assert_eq!(text(&files, ".claude/CLAUDE.md"), "moved");
    assert_eq!(
        json(&files, ".claude.json")["mcpServers"],
        serde_json::json!({"moved": {}})
    );
}
