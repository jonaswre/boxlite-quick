use boxq::Workspace;
use boxq::config::Config;
use boxq::sandbox::Sandboxes;
use boxq::spec::Spec;
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
            let config = Config::from_process_env(&ws)?;
            let spec = Spec::new(config.image.as_deref(), &config.resources)?;
            let up = sandboxes.up(&ws, &spec).await?;
            let did = if up.created {
                "Created"
            } else {
                "Reattached to"
            };
            eprintln!("{did} the Sandbox for {}", ws.path().display());
            let warning = up
                .as_created
                .and_then(|c| spec.mismatch_warning(&c, ws.path()));
            if let Some(warning) = warning {
                eprintln!("{warning}");
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
