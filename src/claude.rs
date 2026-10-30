//! Claude Code in the Sandbox. The interim image ships without it, so a Sandbox installs it on
//! its first `run` and keeps it, until the base image leaves the map's fog with it preinstalled.

use crate::sandbox::ExecTarget;
use anyhow::Context;
use tokio_util::sync::CancellationToken;

/// The release installed, the one boxq's auth is proven with (jonaswre/boxlite-quick#14). The
/// Sandbox's env turns off its self-updates, so it stays at this release.
const PACKAGE: &str = "@anthropic-ai/claude-code@2.1.312";

/// Installs Claude Code in the Sandbox unless it has it, returning whether it installed it.
pub async fn ensure_installed(target: &ExecTarget) -> anyhow::Result<bool> {
    if run_as_root(target, "command -v claude", false).await? == 0 {
        return Ok(false);
    }
    eprintln!("Installing Claude Code in the Sandbox (once)...");
    let install = format!(
        "command -v npm >/dev/null || {{ echo 'the image has no npm' >&2; exit 127; }}
        npm install --global --no-fund --no-audit {PACKAGE}"
    );
    let status = run_as_root(target, &install, true).await?;
    anyhow::ensure!(
        status == 0,
        "cannot install Claude Code in the Sandbox: the install exited with {status}"
    );
    Ok(true)
}

/// Runs `sh -c script` as root, showing its output on stderr if `show`, and returns its exit code.
async fn run_as_root(target: &ExecTarget, script: &str, show: bool) -> anyhow::Result<i32> {
    let token = CancellationToken::new();
    let command = target.command("sh").args(["-c", script]).user("root");
    let mut exec = target
        .session()
        .execution()
        .await?
        .exec(command, token.clone())
        .await?;
    drop(exec.stdin_tx);
    let (mut out, mut err) = (exec.stdout_rx, exec.stderr_rx);
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        let (chunk, from_stdout) = tokio::select! {
            c = out.recv(), if out_open => (c, true),
            c = err.recv(), if err_open => (c, false),
        };
        match chunk {
            Some(chunk) if show => eprint!("{chunk}"),
            Some(_) => {}
            None if from_stdout => out_open = false,
            None => err_open = false,
        }
    }
    let result = exec.result_rx.recv().await;
    token.cancel();
    let result = result.context("the command ended without an exit status")?;
    if let Some(message) = result.error_message {
        anyhow::bail!("the command did not end normally: {message}");
    }
    Ok(result.exit_code)
}
