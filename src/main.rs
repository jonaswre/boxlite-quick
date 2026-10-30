use boxq::Workspace;
use boxq::config::Config;
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

fn main() -> anyhow::Result<()> {
    let (name, target) = match Cli::parse().command {
        Command::Run(t) => ("run", Some(t)),
        Command::Shell(t) => ("shell", Some(t)),
        Command::Ls => ("ls", None),
        Command::Stop(t) => ("stop", Some(t)),
        Command::Rm(t) => ("rm", Some(t)),
    };
    if let Some(target) = target {
        let ws = target.workspace()?;
        eprintln!("Workspace {} (Box {})", ws.path().display(), ws.box_name());
        if matches!(name, "run" | "shell") {
            Config::from_process_env(&ws)?;
        }
    }
    anyhow::bail!("`boxq {name}` is not implemented yet")
}
