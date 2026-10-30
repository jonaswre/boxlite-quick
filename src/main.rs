use boxq::Workspace;
use boxq::auth::Auth;
use boxq::config::Config;
use boxq::sandbox::Sandboxes;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// The image every Sandbox is created from, until the base image leaves the map's fog and the
/// configured image is used (jonaswre/boxlite-quick#18).
const INTERIM_IMAGE: &str = "node:22-bookworm";

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
            let auth = Auth::from_config(&Config::from_process_env(&ws)?)?;
            let up = sandboxes.up(&ws, INTERIM_IMAGE, &auth).await?;
            let did = if up.created {
                "Created"
            } else {
                "Reattached to"
            };
            eprintln!("{did} the Sandbox for {}", ws.path().display());
            if up.auth_changed {
                eprintln!(
                    "warning: the Sandbox still has the base URL and API key it was created \
                     with; run `boxq rm` then `boxq run` to use the current ones"
                );
            }
        }
        Command::Shell(t) => {
            Config::from_process_env(&t.workspace()?)?;
            anyhow::bail!("`boxq shell` is not implemented yet");
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
            sandboxes.rm(&ws).await?;
            eprintln!("Removed the Sandbox for {}", ws.path().display());
        }
    }
    Ok(())
}
