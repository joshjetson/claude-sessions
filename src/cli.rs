//! The subcommands. One binary where the Node app had seven scripts.
//!
//! Everything below `run` is handed a [`Paths`] and a [`ConfigHandle`] built
//! here, once. That is what fixes the Node bug where `notify`, `done` and
//! `blocked` joined their marker paths onto `homedir()` themselves and so
//! ignored `CLAUDE_SESSIONS_HOME`: an isolated run's agents wrote their
//! completion markers into the real dashboard's directory.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::client::{self, DaemonClient};
use crate::daemon::{protocol, server, Engine, EngineOptions};
use crate::paths::Paths;
use crate::term::SpawnPolicy;

mod hooks;
mod journal;
pub(crate) mod markers;
mod pipeline;
mod signals;

/// The daemon's engine, with the outside world wired in where it is
/// configured.
///
/// Every hook is optional on purpose: an install with no Odoo credentials still
/// scans sessions, archives transcripts and raises notifications, and one with
/// no `gitlabHost` does all of that plus the board. The deploy hook is the only
/// one that needs both, because it reads a task's merge request through `glab`.
fn daemon_options(paths: Paths, config: ConfigHandle) -> EngineOptions {
    let mut options = EngineOptions::system(paths, config);
    let creds = options.config.odoo_creds();
    if !creds.is_complete() {
        return options;
    }
    let odoo = Arc::new(crate::odoo::OdooClient::new(creds));
    let gitlab = crate::gitlab::Gitlab::for_config(&options.config, options.spawn);
    options.backend = Arc::new(crate::daemon::OdooTaskBackend::new(
        Arc::clone(&odoo),
        gitlab.clone(),
        options.config.clone(),
    ));
    // Cloned into the hook rather than borrowed: the engine outlives this
    // function, and a deploy fetch happens on a worker thread.
    let specs_config = options.config.clone();
    options.fetch_deploy = Some(Box::new(move || {
        let specs = crate::deploy::deploy_specs(&specs_config);
        let mut board = crate::deploy::fetch_deploy_board(&odoo, &specs)?;
        crate::deploy::enrich_with_live_mrs(&gitlab, &mut board);
        Ok(board)
    }));
    options
}

/// How often the daemon's main thread wakes to notice a signal.
const SIGNAL_POLL: Duration = Duration::from_millis(200);
/// The environment variable a spawned agent carries its task id in.
const TASK_ID_ENV: &str = "CLAUDE_SESSIONS_TASK_ID";

/// A live terminal dashboard for your Claude Code sessions.
#[derive(Parser)]
#[command(name = "claude-sessions", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the background daemon, or ask about one: `daemon status`, `daemon stop`
    Daemon(DaemonArgs),
    /// Report the calling session finished its work
    Done(DoneArgs),
    /// Fire a notification for the calling session
    Notify(NotifyArgs),
    /// Report the calling session is blocked on input
    Blocked(BlockedArgs),
    /// Build and open the reasoning-journal viewer
    Journal(JournalArgs),
    /// Build and open the pipeline viewer, or inspect a pipeline in the terminal
    Pipeline(PipelineArgs),
}

#[derive(Args)]
pub struct JournalArgs {
    /// Print a summary instead of building the page
    #[arg(long)]
    pub stats: bool,
    /// Write the page here instead of `<runtime>/journal.html`
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Write the page without opening it
    #[arg(long = "no-open")]
    pub no_open: bool,
}

#[derive(Args)]
pub struct PipelineArgs {
    #[command(subcommand)]
    pub action: Option<PipelineAction>,
    /// Write the page here instead of `<runtime>/pipeline.html`
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Write the page without opening it
    #[arg(long = "no-open")]
    pub no_open: bool,
}

#[derive(Subcommand)]
pub enum PipelineAction {
    /// Copy the starter override into a repository
    Init {
        repo: PathBuf,
        /// Which built-in pipeline to extend (default: task)
        #[arg(long)]
        pipeline: Option<String>,
    },
    /// List the skills a step can name
    Skills {
        filter: Option<String>,
        /// Also look in this repository's `.claude/skills`
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print one pipeline's steps
    Show {
        id: Option<String>,
        /// Resolve the project's override from this repository
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct DaemonArgs {
    /// `status` or `stop`; omit to run the daemon in the foreground
    pub action: Option<String>,
    /// Bind this port instead of the configured one
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct DoneArgs {
    /// Odoo task id; falls back to CLAUDE_SESSIONS_TASK_ID
    pub task_id: Option<String>,
    /// Read the sign-off summary from this file
    #[arg(long)]
    pub summary_file: Option<PathBuf>,
    /// The sign-off summary itself
    #[arg(long, alias = "summary-text")]
    pub summary: Option<String>,
}

#[derive(Args)]
pub struct BlockedArgs {
    /// Odoo task id; falls back to CLAUDE_SESSIONS_TASK_ID
    pub task_id: Option<String>,
    /// What the reporter needs to answer, separated by `|`
    #[arg(long, alias = "q")]
    pub questions: Option<String>,
}

#[derive(Args)]
pub struct NotifyArgs {
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, alias = "msg")]
    pub message: Option<String>,
    #[arg(long, default_value = "info")]
    pub level: String,
    #[arg(long)]
    pub session: Option<String>,
    /// First positional is the title, the rest are joined into the message
    pub rest: Vec<String>,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    // Paths and config are resolved exactly once, here, and handed down —
    // nothing below reads the environment for itself.
    let paths = Paths::from_env();
    let config = ConfigHandle::load(&paths, EnvOverrides::from_env());
    match cli.command {
        None => crate::ui::run_dashboard(paths, config, SpawnPolicy::detect()),
        Some(Command::Daemon(args)) => daemon(paths, config, args),
        Some(Command::Notify(args)) => markers::notify(&paths, &config, args),
        Some(Command::Done(args)) => markers::done(&paths, args),
        Some(Command::Blocked(args)) => markers::blocked(&paths, args),
        Some(Command::Journal(args)) => journal::run(&paths, &config, args, SpawnPolicy::detect()),
        Some(Command::Pipeline(args)) => {
            pipeline::run(&paths, &config, args, SpawnPolicy::detect())
        }
    }
}

/// Exit the way the helper CLIs always have: a line on stderr and status 1.
fn fail(message: String) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

// --- daemon -----------------------------------------------------------------

fn daemon(paths: Paths, config: ConfigHandle, args: DaemonArgs) -> Result<()> {
    let port = protocol::resolve_port(&config, &paths, args.port);
    match args.action.as_deref() {
        Some("status") => {
            let Some(info) = client::probe(port, client::PROBE_TIMEOUT) else {
                println!("no daemon on :{port}");
                std::process::exit(1);
            };
            println!(
                "daemon running on :{port} (pid {}, up {}s, {} client(s))",
                info.pid,
                info.uptime.round(),
                info.clients
            );
            Ok(())
        }
        Some("stop") => {
            let Some(info) = client::probe(port, client::PROBE_TIMEOUT) else {
                println!("no daemon on :{port}");
                std::process::exit(1);
            };
            if !DaemonClient::new(port).shutdown() {
                fail(format!("could not stop the daemon on :{port}"));
            }
            println!("stopped daemon on :{port} (pid {})", info.pid);
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown daemon command `{other}` — try status or stop"),
        None => run_daemon(paths, config, port),
    }
}

fn run_daemon(paths: Paths, config: ConfigHandle, port: u16) -> Result<()> {
    // Before anything slow: the first refresh scans every process on the
    // machine, and a daemon that cannot be stopped during its own startup gets
    // killed outright — which leaves the discovery file behind pointing at a
    // pid that is gone.
    signals::install();

    // Refuse to double-bind: if one is already up, say so and leave it alone.
    if let Some(existing) = client::probe(port, client::PROBE_TIMEOUT) {
        fail(format!(
            "a daemon is already running on :{port} (pid {})",
            existing.pid
        ));
    }

    // The port comes first, before the engine: a daemon that could not take it
    // has no way to be reached, so it must fail loudly rather than run on as an
    // unreachable process.
    let listener = match server::bind(port) {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => fail(format!(
            "claude-sessions daemon: port {port} is already in use — another dashboard or daemon \
             has it.\n  If that is an older dashboard, quit it (or run `claude-sessions daemon \
             stop`) and retry."
        )),
        Err(error) => fail(format!("claude-sessions daemon: {error}")),
    };

    // One sample a minute into `runtime/memory.log`, and only when asked for.
    let _diagnostics = crate::diagnostics::start(&paths, "daemon", config.diagnostics());

    let mut options = daemon_options(paths.clone(), config);
    options.usage = Some(hooks::usage_hook(&paths, options.spawn));
    options.daily_log = Some(hooks::daily_log_hook(&paths));
    let engine = Arc::new(Engine::new(options));
    let server = server::serve(Arc::clone(&engine), listener)?;
    let _ = protocol::write_daemon_info(&paths, port);
    engine.start();
    println!(
        "claude-sessions daemon listening on 127.0.0.1:{port} (pid {})",
        std::process::id()
    );

    while !server.wait_for_shutdown(SIGNAL_POLL) {
        if signals::caught() {
            println!("shutting down");
            break;
        }
    }

    server.stop();
    engine.stop();
    // Only if it still points at us: a newer daemon may have taken the port.
    protocol::remove_daemon_info(&paths, std::process::id());
    Ok(())
}
