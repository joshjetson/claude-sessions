//! The three helper CLIs a spawned agent calls back with: `notify`, `done` and
//! `blocked`.
//!
//! `done` and `blocked` write marker files the daemon watches — no network at
//! all, so an agent can sign off while the dashboard is closed. `notify` posts
//! to the daemon's loopback port and reports whether it landed.
//!
//! All three take their paths from the [`Paths`] handed down from `run`, which
//! is the fix for the Node bug they all shared: they joined onto `homedir()`
//! themselves, so an isolated run's agents wrote into the real dashboard's
//! directory.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::ConfigHandle;
use crate::daemon::client::DaemonClient;
use crate::daemon::{protocol, BlockedMarker, DoneMarker};
use crate::paths::Paths;
use crate::util::iso_now;

use super::{fail, BlockedArgs, DoneArgs, NotifyArgs, TASK_ID_ENV};

// --- notify -----------------------------------------------------------------

pub(super) fn notify(paths: &Paths, config: &ConfigHandle, args: NotifyArgs) -> Result<()> {
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

pub(super) fn done(paths: &Paths, args: DoneArgs) -> Result<()> {
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

pub(super) fn blocked(paths: &Paths, args: BlockedArgs) -> Result<()> {
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
