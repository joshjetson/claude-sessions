//! The three things the engine notices on your behalf.
//!
//! - a session waiting on a decision you have not been told about;
//! - a running session that has gone quiet;
//! - a task landing in a stage that means it is yours now.
//!
//! The fourth — a session that went away without saying so — is the
//! `vanished` module, because archiving is a different kind of work from
//! raising a notification.

use std::time::{Duration, SystemTime};

use crate::scan::ProcessSource;
use crate::types::{EntryKind, LastEntry, NotificationLevel, Session, SessionStatus};
use crate::util::project_name;

use super::alerts::{
    detect_new_assignments, detect_stalls, human_duration, last_write, StallOptions,
};
use super::engine::EngineInner;
use super::notify::NewNotification;
use super::state::SessionIndex;

/// Marks that the assignment watcher has seen the board at least once.
const BOOTSTRAP_KEY: &str = "alerts:bootstrapped";

/// How long a pending tool call must sit before it is read as "blocked on you".
///
/// A permission prompt is NOT `AskUserQuestion` — it is an ordinary tool call
/// that never gets its result, so [`is_awaiting_user_decision`] cannot see it
/// and the session waits in silence until the fifteen-minute stall sweep
/// notices. That is a quarter of an hour of a QA run doing nothing because
/// nothing knew it was blocked.
///
/// The cost of the broader check is false positives on genuinely slow tools: a
/// test run, a build, a browser step. Two minutes clears almost all of those
/// while still being far better than fifteen. A false "this might want you" is
/// cheap; a real block nobody hears is not.
pub const BLOCKED_TOOL_DWELL: Duration = Duration::from_secs(120);

/// A session is awaiting a user decision when its newest transcript entry is
/// the agent asking a question or presenting a plan, with no answer yet.
pub fn is_awaiting_user_decision(last_entry: Option<&LastEntry>) -> bool {
    let Some(entry) = last_entry else {
        return false;
    };
    entry.kind == EntryKind::Assistant
        && entry
            .tool_uses
            .iter()
            .any(|name| name == "AskUserQuestion" || name == "ExitPlanMode")
}

/// A pending tool call with no result yet, quiet long enough to look stuck
/// rather than slow.
///
/// [`crate::util::detect_session_status`] already calls this state `awaiting` —
/// this only adds the dwell time, so the two cannot disagree about what it
/// means.
pub fn is_blocked_on_tool_call(session: &Session, now: SystemTime) -> bool {
    if session.status != SessionStatus::Awaiting {
        return false;
    }
    last_write(session)
        .map(|mtime| now.duration_since(mtime).unwrap_or_default())
        .is_some_and(|silent| silent >= BLOCKED_TOOL_DWELL)
}

impl<S: ProcessSource> EngineInner<S> {
    /// Alert when a session is waiting on YOU to choose. Agents do not run the
    /// notify hook for these, so without this they wait silently.
    ///
    /// Edge-triggered per session: raised when the wait starts, cleared when it
    /// ends, so a session sitting on a question does not notify once a second.
    pub(crate) fn notify_awaiting_decisions(&self, sessions: &SessionIndex, now: SystemTime) {
        let mut raise = Vec::new();
        {
            let mut state = self.state();
            for session in sessions.iter() {
                if session.session_id.is_empty() {
                    continue;
                }
                let asked = is_awaiting_user_decision(session.last_entry.as_ref());
                let blocked = !asked && is_blocked_on_tool_call(session, now);
                let already = state.await_notified.contains(&session.session_id);
                match (asked || blocked, already) {
                    (true, false) => {
                        state.await_notified.insert(session.session_id.clone());
                        raise.push(awaiting_notification(session, asked));
                    }
                    (false, true) => {
                        state.await_notified.remove(&session.session_id);
                    }
                    _ => {}
                }
            }
        }
        for notification in raise {
            self.push_notification(notification);
        }
    }

    /// Flag a running task session that has gone quiet.
    ///
    /// [`Self::notify_awaiting_decisions`] only fires when the newest entry is
    /// literally a question. This catches the rest: a hung command, a crashed
    /// agent — anything that leaves a session silent while you assume it is
    /// working.
    pub(crate) fn notify_stalled_sessions(&self, sessions: &SessionIndex, now: SystemTime) {
        let alerts = self.config().alerts();
        if !alerts.enabled || alerts.stuck_after.is_zero() {
            return;
        }

        let mut raise = Vec::new();
        {
            let mut state = self.state();

            // A session that started moving again clears its flag, so the next
            // stall is reported afresh rather than suppressed by the previous
            // one.
            let resumed: Vec<i64> = state
                .stall_alerted_at
                .keys()
                .copied()
                .filter(|task_id| {
                    state
                        .task_sessions
                        .get(task_id)
                        .and_then(|link| sessions.get(&link.session_id))
                        .and_then(last_write)
                        .is_some_and(|mtime| {
                            now.duration_since(mtime).unwrap_or_default() < alerts.stuck_after
                        })
                })
                .collect();
            for task_id in resumed {
                state.stall_alerted_at.remove(&task_id);
            }

            let options = StallOptions {
                now,
                stuck_after: alerts.stuck_after,
                remind_every: alerts.remind_every,
            };
            let stalls = detect_stalls(
                &state.task_sessions,
                |session_id| sessions.get(session_id),
                &options,
                &state.stall_alerted_at,
            );

            for stall in &stalls {
                let known = state.task(stall.task_id);
                let project = known
                    .map(|task| task.project_name.clone())
                    .unwrap_or_default();
                let where_it_is = match known {
                    Some(task) => format!("{} — {}", task.project_name, task.name),
                    None => stall.cwd.clone(),
                };
                let silent = human_duration(stall.silent);
                raise.push(NewNotification {
                    cwd: stall.cwd.clone(),
                    project: Some(project),
                    task_id: Some(stall.task_id),
                    level: NotificationLevel::Warn,
                    message: if stall.awaiting {
                        format!("{where_it_is}. Claude looks like it is waiting on you — open its terminal (g) to check.")
                    } else {
                        format!("{where_it_is}. No output for {silent} — it may be stuck, or waiting on a decision. Press g to look.")
                    },
                    ..NewNotification::new(
                        "stalled",
                        format!("⏳ Task #{} quiet for {silent}", stall.task_id),
                        String::new(),
                    )
                });
            }
            for stall in stalls {
                state.stall_alerted_at.insert(stall.task_id, now);
            }
        }
        for notification in raise {
            self.push_notification(notification);
        }
    }

    /// Tell me when something lands in my queue, without me refreshing the
    /// board.
    ///
    /// Already-announced tasks are remembered in SQLite, so restarting the
    /// daemon does not re-announce everything sitting in the stage.
    pub(crate) fn notify_new_assignments(&self) {
        let alerts = self.config().alerts();
        if !alerts.enabled || alerts.new_task_stages.is_empty() {
            return;
        }
        let Some(fetch) = &self.fetch_assigned else {
            return;
        };
        // Best-effort: an Odoo blip must not kill the loop.
        let Ok(tasks) = fetch(&alerts.new_task_stages) else {
            return;
        };

        let fresh = detect_new_assignments(&tasks, &alerts.new_task_stages, |key| {
            self.db.was_alerted(key)
        });

        // First run: everything already sitting in the stage is not news.
        // Record it silently, so you are told about genuinely new arrivals from
        // here on rather than being handed your entire backlog at once.
        if !self.db.was_alerted(BOOTSTRAP_KEY) {
            for assignment in &fresh {
                self.db.mark_alerted(&assignment.key);
            }
            self.db.mark_alerted(BOOTSTRAP_KEY);
            return;
        }

        for assignment in fresh {
            self.db.mark_alerted(&assignment.key);
            let task = assignment.task;
            self.push_notification(NewNotification {
                project: Some(task.project_name.clone()),
                task_id: Some(task.id),
                level: NotificationLevel::Info,
                ..NewNotification::new(
                    "assigned",
                    format!("📥 Assigned: #{} {}", task.id, task.name),
                    format!(
                        "{} — now in {}. Press s on the board to start it.",
                        task.project_name, task.stage_name
                    ),
                )
            });
        }
    }
}

fn awaiting_notification(session: &Session, asked: bool) -> NewNotification {
    let project = project_name(&session.cwd);
    let (title, message) = if asked {
        (
            format!("🔔 {project}: Claude needs your decision"),
            "Claude is waiting for you to choose an option. Open its terminal (press o on the session) to answer.",
        )
    } else {
        (
            format!("🔔 {project}: Claude may be waiting on a prompt"),
            "A tool call has been pending for two minutes — most likely a permission prompt. Open its terminal (press o on the session) to look.",
        )
    };
    NewNotification {
        cwd: session.cwd.clone(),
        project: Some(project),
        session_id: Some(session.session_id.clone()),
        level: NotificationLevel::Warn,
        ..NewNotification::new("await", title, message)
    }
}
