use clap::{Parser, Subcommand};

use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::Paths;
use crate::term::SpawnPolicy;

/// A live terminal dashboard for your Claude Code sessions.
#[derive(Parser)]
#[command(name = "claude-sessions", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the background daemon that watches sessions and fires alerts
    Daemon,
    /// Hook endpoint: report the calling session finished its work
    Done,
    /// Hook endpoint: fire a notification for the calling session
    Notify,
    /// Hook endpoint: report the calling session is blocked on input
    Blocked,
    /// Open the journal viewer
    Journal,
    /// Open the pipeline viewer
    Pipeline,
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Paths and config are resolved exactly once, here, and handed down —
    // nothing below `main` reads the environment for itself.
    let surface = match cli.command {
        None => {
            let paths = Paths::from_env();
            let config = ConfigHandle::load(&paths, EnvOverrides::from_env());
            return crate::ui::run_dashboard(paths, config, SpawnPolicy::detect());
        }
        Some(Command::Daemon) => "daemon",
        Some(Command::Done) => "done hook",
        Some(Command::Notify) => "notify hook",
        Some(Command::Blocked) => "blocked hook",
        Some(Command::Journal) => "journal viewer",
        Some(Command::Pipeline) => "pipeline viewer",
    };
    anyhow::bail!("the {surface} has not landed yet — porting is underway, follow along at https://github.com/joshjetson/claude-sessions")
}
