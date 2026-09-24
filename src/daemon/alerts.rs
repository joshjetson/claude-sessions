//! Alerts the daemon raises on its own, without you watching the board.
//!
//! Two things that previously needed you to be looking:
//!
//! 1. A task landing in a stage that means "this is yours now". You only found
//!    out by refreshing the board.
//! 2. A running task session going quiet. The awaiting-decision check only
//!    fires when the newest transcript entry is literally a question — it
//!    misses an agent stuck on a hung command, a permission prompt, or simply
//!    nothing. A session silent for fifteen minutes is worth a look whatever
//!    the reason.
//!
//! Everything here is pure, so it can be tested without Odoo, without a daemon
//! and without waiting fifteen real minutes. The Odoo lookup that feeds
//! [`detect_new_assignments`] is injected for the same reason.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::state::TaskLink;
use crate::types::{Session, SessionStatus, Task};

/// Stage-name comparison is case- and whitespace-insensitive: people type these
/// into config by hand, and Odoo renders them with its own capitalisation.
pub fn normalise_stage(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A task newly arrived in a watched stage, with the key that records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAssignment<'a> {
    pub task: &'a Task,
    pub key: String,
}

/// Tasks newly arrived in a watched stage.
///
/// `was_alerted` is the bookkeeping side — [`crate::db::Db::was_alerted`] in
/// production, a set in tests.
pub fn detect_new_assignments<'a>(
    tasks: &'a [Task],
    stages: &[String],
    was_alerted: impl Fn(&str) -> bool,
) -> Vec<NewAssignment<'a>> {
    let watched: Vec<String> = stages.iter().map(|s| normalise_stage(s)).collect();
    let mut out = Vec::new();
    for task in tasks {
        // A row with no id is malformed, not a task: skipped rather than fatal,
        // because this list comes off the network.
        if task.id == 0 {
            continue;
        }
        let stage = normalise_stage(&task.stage_name);
        if !watched.contains(&stage) {
            continue;
        }
        // Keyed by stage as well as task: a task that goes back to "Approved to
        // Start" after a revision is genuinely new work again, but re-entering
        // the same stage twice running is not announced twice.
        let key = format!("assigned:{}:{stage}", task.id);
        if was_alerted(&key) {
            continue;
        }
        out.push(NewAssignment { task, key });
    }
    out
}

/// How long a session must have been silent, and how often to say so again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallOptions {
    pub now: SystemTime,
    pub stuck_after: Duration,
    /// Zero means "flag it once, never remind".
    pub remind_every: Duration,
}

/// A running task session that has gone quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub task_id: i64,
    pub session_id: String,
    pub cwd: String,
    pub silent: Duration,
    /// The awaiting-a-decision signal, when we have it, explains WHY.
    pub awaiting: bool,
}

/// Running task sessions that have gone quiet.
///
/// `session` is the lookup from a session id to the live session — a map, an
/// index, or a closure over a slice; the rules do not care which.
/// `last_alerted` is task id -> when it was last flagged.
pub fn detect_stalls<'a>(
    links: &BTreeMap<i64, TaskLink>,
    session: impl Fn(&str) -> Option<&'a Session>,
    options: &StallOptions,
    last_alerted: &HashMap<i64, SystemTime>,
) -> Vec<Stall> {
    let mut out = Vec::new();
    for (task_id, link) in links {
        // Only sessions we believe are still working. A task that finished, was
        // archived or hit the readiness gate is not stalled.
        if !link.is_running() || link.session_id.is_empty() {
            continue;
        }
        // Vanished — auto-archiving handles that path separately.
        let Some(session) = session(&link.session_id) else {
            continue;
        };
        let Some(mtime) = last_write(session) else {
            continue;
        };
        let silent = options.now.duration_since(mtime).unwrap_or_default();
        if silent < options.stuck_after {
            continue;
        }
        // Already flagged? Only repeat once the reminder interval has passed,
        // and never repeat at all when reminders are switched off.
        if let Some(previous) = last_alerted.get(task_id) {
            if options.remind_every.is_zero() {
                continue;
            }
            if options.now.duration_since(*previous).unwrap_or_default() < options.remind_every {
                continue;
            }
        }
        out.push(Stall {
            task_id: *task_id,
            session_id: link.session_id.clone(),
            cwd: session.cwd.clone(),
            silent,
            // Only a real hand-back: a question, a plan, or a permission
            // prompt. `AwaitingInput` meant "the person typed and nothing came
            // back", which is the agent thinking or dead, not waiting on you.
            // Nothing produces it any more, and it must not word a stall as
            // "waiting on you" if an older daemon's state still carries it.
            awaiting: session.status == SessionStatus::Awaiting,
        });
    }
    out
}

/// When a session's transcript was last written, or `None` when we do not know.
///
/// A transcript stamped at the epoch is a `stat` that failed, not a session
/// silent since 1970 — the Node app stored `0` for the same case and skipped
/// it, rather than reporting it as infinitely stalled.
pub(crate) fn last_write(session: &Session) -> Option<SystemTime> {
    (session.session_mtime != UNIX_EPOCH).then_some(session.session_mtime)
}

/// "18 minutes" / "1 hour 5 minutes" — for the notification text. Rounds down,
/// so a session quiet for 119 seconds reads as one minute rather than two.
pub fn human_duration(duration: Duration) -> String {
    let total_minutes = duration.as_secs() / 60;
    if total_minutes < 60 {
        return format!("{total_minutes} minute{}", plural(total_minutes));
    }
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    let head = format!("{hours} hour{}", plural(hours));
    if minutes == 0 {
        head
    } else {
        format!("{head} {minutes} minute{}", plural(minutes))
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
