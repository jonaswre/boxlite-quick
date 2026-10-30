use boxq::Workspace;
use boxq::auth::Auth;
use boxq::claude;
use boxq::claude_config::ClaudeConfig;
use boxq::config::Config;
use boxq::egress::{Approvals, Policy};
use boxq::sandbox::{NoSandbox, Sandboxes, Up};
use boxq::session;
use boxq::spec::{self, Spec};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Run Claude Code against a Workspace inside an isolated Sandbox.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or reattach to the Workspace's Sandbox and start Claude Code in it.
    Run(Target),
    /// Open a shell in the Workspace's Sandbox.
    Shell(Target),
    /// List Sandboxes by Workspace path.
    Ls,
    /// Stop the Workspace's Sandbox, keeping its state.
    Stop(Target),
    /// Remove the Workspace's Sandbox and its state.
    Rm(Target),
}

#[derive(clap::Args)]
struct Target {
    /// The Workspace; defaults to the current directory.
    path: Option<PathBuf>,
}

impl Target {
    fn workspace(&self) -> anyhow::Result<Workspace> {
        Workspace::resolve(self.path.as_deref().unwrap_or(".".as_ref()))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sandboxes = Sandboxes::from_process_env()?;
    match Cli::parse().command {
        Command::Run(t) => {
            let ws = t.workspace()?;
            let claude_config = ClaudeConfig::from_process_env()?;
            let sandbox = ready_sandbox(&sandboxes, &ws).await?;
            claude::ensure_installed(&sandbox.target).await?;
            claude_config.sync(&ws, &sandbox.target).await?;
            let status = session::interactive(&sandbox.target, &ws, session::CLAUDE).await?;
            std::process::exit(status);
        }
        Command::Shell(t) => {
            let ws = t.workspace()?;
            let sandbox = ready_sandbox(&sandboxes, &ws).await?;
            let status = session::interactive(&sandbox.target, &ws, session::LOGIN_SHELL).await?;
            std::process::exit(status);
        }
        Command::Ls => {
            for s in sandboxes.list().await? {
                let state = if s.running { "running" } else { "stopped" };
                match s.workspace {
                    Some(ws) => println!("{state}  {}", ws.display()),
                    None => println!("{state}  (Workspace unknown; Box {})", s.box_name),
                }
            }
        }
        Command::Stop(t) => {
            let ws = t.workspace()?;
            sandboxes.stop(&ws).await?;
            eprintln!("Stopped the Sandbox for {}", ws.path().display());
        }
        Command::Rm(t) => {
            let ws = t.workspace()?;
            let removed = sandboxes.rm(&ws).await;
            // Forgotten even if there is no Sandbox, so `rm` always resets the Workspace, but
            // kept if removing the Sandbox failed, since it may still be there.
            let gone = match &removed {
                Ok(()) => true,
                Err(e) => e.is::<NoSandbox>(),
            };
            if gone {
                Approvals::from_process_env()?.revoke(&ws)?;
            }
            removed?;
            eprintln!("Removed the Sandbox for {}", ws.path().display());
        }
    }
    Ok(())
}

/// Creates the Workspace's Sandbox from the config, or reattaches to it, saying which and
/// warning where the Sandbox no longer matches the config.
async fn ready_sandbox(sandboxes: &Sandboxes, ws: &Workspace) -> anyhow::Result<Up> {
    let config = Config::from_process_env(ws)?;
    let auth = Auth::from_config(&config)?;
    let spec = Spec::new(config.image.as_deref(), &config.resources)?;
    let egress = Policy::from_process_env(ws, &config)?;
    let up = sandboxes.up(ws, &spec, &egress, &auth).await?;
    let action = if up.created {
        "Created"
    } else {
        "Reattached to"
    };
    eprintln!("{action} the Sandbox for {}", ws.path().display());
    let warning = up
        .as_created
        .as_ref()
        .and_then(|c| spec.mismatch_warning(c, ws.path()));
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    if up.auth_changed {
        eprintln!(
            "The Sandbox for {} still has the base URL and API key it was created with; to \
             use the current ones, {}.",
            ws.path().display(),
            spec::recreate_hint(ws.path())
        );
    }
    Ok(up)
}
