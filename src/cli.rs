//! The subcommands. One binary where the Node app had seven scripts.
//!
//! Everything below `run` is handed a [`Paths`] and a [`ConfigHandle`] built
//! here, once. That is what fixes the Node bug where `notify`, `done` and
//! `blocked` joined their marker paths onto `homedir()` themselves and so
//! ignored `CLAUDE_SESSIONS_HOME`: an isolated run's agents wrote their
//! completion markers into the real dashboard's directory.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::client::{self, DaemonClient};
use crate::daemon::{protocol, server, BlockedMarker, DoneMarker, Engine, EngineOptions};
use crate::paths::Paths;
use crate::term::SpawnPolicy;
use crate::util::iso_now;

mod signals;

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
    /// Open the journal viewer
    Journal,
    /// Open the pipeline viewer
    Pipeline,
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
        Some(Command::Notify(args)) => notify(&paths, &config, args),
        Some(Command::Done(args)) => done(&paths, args),
        Some(Command::Blocked(args)) => blocked(&paths, args),
        Some(Command::Journal) => unlanded("journal viewer"),
        Some(Command::Pipeline) => unlanded("pipeline viewer"),
    }
}

fn unlanded(surface: &str) -> Result<()> {
    anyhow::bail!("the {surface} has not landed yet — porting is underway, follow along at https://github.com/joshjetson/claude-sessions")
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

    let engine = Arc::new(Engine::new(EngineOptions::system(paths.clone(), config)));
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

// --- notify -----------------------------------------------------------------

fn notify(paths: &Paths, config: &ConfigHandle, args: NotifyArgs) -> Result<()> {
    let (title, message) = notify_text(args.title, args.message, args.rest);
    if title.is_empty() && message.is_empty() {
        fail("notify: provide a title and/or message".to_string());
    }
    let port = protocol::resolve_port(config, paths, None);
    let body = serde_json::json!({
        "title": if title.is_empty() { "Notification".to_string() } else { title },
        "message": message,
        "level": args.level,
        "cwd": cwd(),
        "taskId": task_id_from_env(),
        "sessionId": args.session.unwrap_or_default(),
    });
    match DaemonClient::new(port).notify(body) {
        Some(response) => {
            println!("notify: sent ({})", response.status);
            Ok(())
        }
        None => fail(format!(
            "notify: dashboard not reachable on 127.0.0.1:{port}"
        )),
    }
}

/// The positional fallback: the first bare argument is the title and whatever
/// follows is the message, so `notify "Need a decision" "Postgres or SQLite?"`
/// works with no flags at all.
pub fn notify_text(
    title: Option<String>,
    message: Option<String>,
    rest: Vec<String>,
) -> (String, String) {
    let mut rest = rest.into_iter();
    let title = title.unwrap_or_else(|| rest.next().unwrap_or_default());
    let message = message.unwrap_or_else(|| rest.collect::<Vec<_>>().join(" "));
    (title, message)
}

// --- done and blocked -------------------------------------------------------

fn done(paths: &Paths, args: DoneArgs) -> Result<()> {
    let task_id = require_task_id("done", args.task_id);
    let mut summary = args.summary.unwrap_or_default();
    if summary.is_empty() {
        if let Some(file) = args.summary_file {
            summary = std::fs::read_to_string(&file).unwrap_or_else(|_| {
                eprintln!(
                    "done: could not read summary file {} (continuing without it)",
                    file.display()
                );
                String::new()
            });
        }
    }
    let marker = write_done_marker(paths, task_id, &cwd(), &summary)?;
    let note = if summary.is_empty() {
        ""
    } else {
        " (with summary)"
    };
    println!("done: marked task {task_id}{note} ({})", marker.display());
    Ok(())
}

fn blocked(paths: &Paths, args: BlockedArgs) -> Result<()> {
    let task_id = require_task_id("blocked", args.task_id);
    let questions = split_questions(&args.questions.unwrap_or_default());
    let marker = write_blocked_marker(paths, task_id, &cwd(), questions)?;
    println!(
        "blocked: flagged task {task_id} as needs-info ({})",
        marker.display()
    );
    Ok(())
}

/// Write `done/<taskId>.json`. No network: the daemon watches the directory, so
/// a sign-off works with nothing running and is picked up when something is.
pub fn write_done_marker(paths: &Paths, task_id: i64, cwd: &str, summary: &str) -> Result<PathBuf> {
    let marker = DoneMarker {
        task_id,
        cwd: cwd.to_string(),
        summary: summary.to_string(),
        ts: iso_now(),
    }
    // The same cap the daemon applies on read: an 8 MB paste must not become
    // an 8 MB Odoo comment.
    .capped();
    write_marker(&paths.done_marker(task_id), &marker)
}

pub fn write_blocked_marker(
    paths: &Paths,
    task_id: i64,
    cwd: &str,
    questions: Vec<String>,
) -> Result<PathBuf> {
    let marker = BlockedMarker {
        task_id,
        cwd: cwd.to_string(),
        questions,
        ts: iso_now(),
    };
    write_marker(&paths.blocked_marker(task_id), &marker)
}

fn write_marker(path: &Path, marker: &impl serde::Serialize) -> Result<PathBuf> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(marker)?)?;
    Ok(path.to_path_buf())
}

/// `"q1 | q2 | q3"` into three questions.
pub fn split_questions(raw: &str) -> Vec<String> {
    raw.split('|')
        .map(|question| question.trim().to_string())
        .filter(|question| !question.is_empty())
        .collect()
}

/// The task id, from the argument or the environment the agent was spawned
/// with. Without one there is nothing to mark, which is an error.
fn require_task_id(command: &str, argument: Option<String>) -> i64 {
    let raw = argument.or_else(task_id_text).unwrap_or_else(|| {
        fail(format!(
            "{command}: no task id (pass as argument or set {TASK_ID_ENV})"
        ))
    });
    raw.trim()
        .parse()
        .unwrap_or_else(|_| fail(format!("{command}: `{raw}` is not a task id")))
}

fn task_id_text() -> Option<String> {
    std::env::var(TASK_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn task_id_from_env() -> Option<i64> {
    task_id_text()?.trim().parse().ok()
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default()
}
