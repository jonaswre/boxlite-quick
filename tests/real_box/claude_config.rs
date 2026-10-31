//! Syncing the host's Claude config into a Sandbox: `src/claude_config.rs`.

use super::*;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "boots a real Box; needs KVM"]
async fn the_hosts_claude_config_merges_into_the_sandboxs_own() {
    let mut f = Fixture::new();
    let host = host_claude_config(&f.temp_dir(), &f.workspace);
    let up = f.up(IMAGE).await;
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
    let up = f.up(IMAGE).await;

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
    let up = f.up(IMAGE).await;

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
    let up = f.up(NON_ROOT_IMAGE).await;
    let (user, _) = sh(&up.target, "id -u").await.unwrap();
    assert_ne!(user, "0\n", "the image's user is root");

    host.sync(&f.workspace, &up.target).await.unwrap();

    let synced = root_sh(&up.target, "cat /root/.claude/skills/review/SKILL.md")
        .await
        .unwrap();
    assert_eq!(synced, ("# Review\n".into(), 0));
}
