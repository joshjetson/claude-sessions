//! The things the engine notices on your behalf.
//!
//! - a session waiting on a decision you have not been told about;
//! - a running session that has gone quiet (per task, or for the QA role one
//!   aggregated row for every live session);
//! - a task landing in a stage that means it is yours now (for the QA role, a
//!   task landing in a QA stage).
//!
//! The fourth — a session that went away without saying so — is the
//! `vanished` module, because archiving is a different kind of work from
//! raising a notification.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use crate::scan::ProcessSource;
use crate::types::{EntryKind, LastEntry, NotificationLevel, Session, SessionStatus};
use crate::util::project_name;

use super::alerts::{
    detect_new_assignments, detect_qa_arrivals, detect_quiet_sessions, detect_stalls,
    human_duration, last_write, quiet_sessions_text, StallOptions,
};
use super::engine::EngineInner;
use super::notify::NewNotification;
use super::state::{QuietStep, SessionIndex};

/// Marks that the assignment watcher has seen the board at least once.
const BOOTSTRAP_KEY: &str = "alerts:bootstrapped";
/// The same for the QA role's arrival watcher. Its own key, so turning the role
/// on for the first time records the QA stages silently even on a machine whose
/// assignment watcher bootstrapped long ago.
const QA_BOOTSTRAP_KEY: &str = "qa-new:bootstrapped";

/// How long a pending tool call must sit before it is read as "blocked on you",
/// for a session with no hook state.
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
/// cheap; a real block nobody hears is not. With the hooks installed there is
/// no guess and no dwell: see [`is_blocked_on_tool_call`].
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

/// The session is most likely stopped on a permission prompt.
///
/// Two ways to know, depending on whether the hooks are installed (`hooked`
/// is true when a hook state file exists for the session):
///
/// - With hooks, the status says so. `awaiting` that is not a question or a
///   plan can only come from a `PermissionRequest` or a `permission_prompt`
///   notification, so it counts at once.
/// - Without hooks, a pending ordinary tool call quiet for
///   [`BLOCKED_TOOL_DWELL`] counts. The quiet is measured from the
///   conversation's newest line, because hook results and other bookkeeping
///   touch the file's mtime on their own.
///
/// This used to require the status `awaiting` AND an ordinary tool call. The
/// status machine only ever gave `awaiting` to a question, which the caller
/// already excludes, so the notification could never fire.
pub fn is_blocked_on_tool_call(session: &Session, hooked: bool, now: SystemTime) -> bool {
    let asked = is_awaiting_user_decision(session.last_entry.as_ref());
    if session.status == SessionStatus::Awaiting {
        return !asked;
    }
    // With hooks, a pending tool call that no hook called a prompt is a tool
    // that is running. Guessing from silence would only add false alarms.
    if hooked || session.status != SessionStatus::Working || asked {
        return false;
    }
    let Some(entry) = session.last_entry.as_ref() else {
        return false;
    };
    if entry.kind != EntryKind::Assistant || !entry.has_tool_use() {
        return false;
    }
    entry
        .activity_instant()
        .or_else(|| last_write(session))
        .map(|since| now.duration_since(since).unwrap_or_default())
        .is_some_and(|silent| silent >= BLOCKED_TOOL_DWELL)
}

impl<S: ProcessSource> EngineInner<S> {
    /// Alert when a session is waiting on YOU to choose. Agents do not run the
    /// notify hook for these, so without this they wait silently.
    ///
    /// Edge-triggered per session: raised when the wait starts, cleared when it
    /// ends, so a session sitting on a question does not notify once a second.
    ///
    /// `hooked` names the sessions that have a hook state file. See
    /// [`is_blocked_on_tool_call`].
    pub(crate) fn notify_awaiting_decisions(
        &self,
        sessions: &SessionIndex,
        hooked: &HashSet<String>,
        now: SystemTime,
    ) {
        let mut raise = Vec::new();
        let mut ended = Vec::new();
        {
            let mut state = self.state();
            for session in sessions.iter() {
                if session.session_id.is_empty() {
                    continue;
                }
                let asked = is_awaiting_user_decision(session.last_entry.as_ref());
                let blocked = !asked
                    && is_blocked_on_tool_call(session, hooked.contains(&session.session_id), now);
                let already = state.await_notified.contains(&session.session_id);
                match (asked || blocked, already) {
                    (true, false) => {
                        state.await_notified.insert(session.session_id.clone());
                        raise.push(awaiting_notification(session, asked));
                    }
                    (false, true) => {
                        state.await_notified.remove(&session.session_id);
                        ended.push(session.session_id.clone());
                    }
                    _ => {}
                }
            }
        }
        // The wait ended, so the question was answered, wherever that
        // happened. Clearing it here is what takes "← asks you" off the board.
        self.resolve_answered_questions(&ended);
        for notification in raise {
            self.raise_notification(notification);
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
        // The QA role gets one aggregated row instead. See
        // [`Self::update_quiet_sessions`].
        if !self
            .config()
            .role()
            .notification_policy()
            .per_task_stall_alerts()
        {
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
            self.raise_notification(notification);
        }
    }

    /// Keep the QA role's one quiet-sessions row current.
    ///
    /// The per-task stall alert raises one notification per task and repeats
    /// it every reminder interval. A reviewer with ten sessions who steps away
    /// for an hour came back to dozens of rows and as many sounds. This is one
    /// row with a fixed id, updated in place: "4 sessions quiet 15m+", plus the
    /// longest. It rings once, when it first appears, and it goes away by
    /// itself when every session is writing again.
    ///
    /// It covers every live session, not only the ones linked to a task,
    /// because a reviewer starts most QA sessions by hand.
    pub(crate) fn update_quiet_sessions(&self, sessions: &SessionIndex, now: SystemTime) {
        let (policy, alerts) = {
            let config = self.config();
            (config.role().notification_policy(), config.alerts())
        };
        if !policy.quiet_sessions_row() || !alerts.enabled || alerts.stuck_after.is_zero() {
            // The role changed, or alerts were switched off: take the row away
            // and forget any dismissal, so turning it back on starts clean.
            if self.state().quiet.reset() {
                self.remove_quiet_row();
            }
            return;
        }
        let quiet = detect_quiet_sessions(sessions.iter(), now, alerts.stuck_after);
        let ids = quiet.iter().map(|q| q.session_id.clone()).collect();
        let step = self.state().quiet.plan(ids);
        match step {
            QuietStep::Stay => {}
            QuietStep::Remove => self.remove_quiet_row(),
            QuietStep::Raise | QuietStep::Update => {
                let (title, message) = quiet_sessions_text(&quiet, alerts.stuck_after);
                if step == QuietStep::Raise {
                    self.raise_quiet_row(title, message);
                } else {
                    self.update_quiet_row(title, message);
                }
            }
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
            self.raise_notification(NewNotification {
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

impl<S: ProcessSource> EngineInner<S> {
    /// Tell a QA reviewer when a task lands in a QA stage.
    ///
    /// The QA Board's rule, run by the daemon so it works with no browser tab
    /// open. See [`detect_qa_arrivals`] for what counts. Arrivals are recorded
    /// in SQLite like the assignment watcher's, so a restart announces what
    /// arrived while the daemon was down, and nothing twice.
    ///
    /// It runs only for the QA role, because it costs an Odoo query every slow
    /// tick. When it has been skipped for another role, the next QA tick
    /// records the stages silently, exactly like the first run ever: switching
    /// role must not replay a week of arrivals.
    pub(crate) fn notify_qa_arrivals(&self) {
        let (policy, enabled, rule) = {
            let config = self.config();
            (
                config.role().notification_policy(),
                config.alerts().enabled,
                config.qa_alerts(),
            )
        };
        if !policy.watches_qa_arrivals() {
            self.state().qa_watch_paused = true;
            return;
        }
        if !enabled || rule.stages.is_empty() {
            return;
        }
        let Some(fetch) = &self.fetch_qa_stage else {
            return;
        };
        // Best-effort: an Odoo blip must not kill the loop.
        let Ok(tasks) = fetch(&rule.stages) else {
            return;
        };

        let fresh = detect_qa_arrivals(&tasks, &rule, |key| self.db.was_alerted(key));

        let resume = std::mem::take(&mut self.state().qa_watch_paused);
        if resume || !self.db.was_alerted(QA_BOOTSTRAP_KEY) {
            for arrival in &fresh {
                self.db.mark_alerted(&arrival.key);
            }
            self.db.mark_alerted(QA_BOOTSTRAP_KEY);
            return;
        }

        for arrival in fresh {
            self.db.mark_alerted(&arrival.key);
            if !arrival.announce {
                continue;
            }
            let entry = arrival.entry;
            let task = &entry.task;
            let title = if entry.assigned_to_me {
                format!("🧪 Assigned to you · #{} {}", task.id, task.name)
            } else {
                format!("🧪 New in QA: #{} {}", task.id, task.name)
            };
            self.raise_notification(NewNotification {
                project: Some(task.project_name.clone()),
                task_id: Some(task.id),
                level: NotificationLevel::Info,
                ..NewNotification::new(
                    "qa-new",
                    title,
                    format!(
                        "{} — now in {}. Press Enter on it in the board and pick QA to start a pass.",
                        task.project_name, task.stage_name
                    ),
                )
            });
        }
    }
}

/// The notification for a session that has stopped and is waiting on a person.
///
/// `asked` separates two states that look alike on the board and are not alike
/// at all:
///
///  * TRUE — the agent called AskUserQuestion or ExitPlanMode. It handed control
///    back deliberately, and what it wants is an answer. That is a `Question`,
///    and it names its task, so the dashboard can answer it in the reviewer's
///    name and a run can count it as blocked. Until this, those numbered
///    prompts were raised as `info` with no task id, which made them
///    unanswerable by the coordinator and invisible to the reviewer's own relay
///    — the reviewer had to find the pane and type into it.
///
///  * FALSE — a permission prompt, reported by a hook, or guessed from a tool
///    call pending for two minutes when no hook is installed. That is a modal inside this tool's own UI: it is not
///    in the transcript, nothing can answer it remotely, and only the person at
///    the keyboard can clear it. It stays `info`, because marking it answerable
///    would invite the coordinator to try and be refused.
pub(crate) fn awaiting_notification(session: &Session, asked: bool) -> NewNotification {
    let project = project_name(&session.cwd);
    if asked {
        let title = format!("🔔 {project}: Claude needs your decision");
        return NewNotification {
            cwd: session.cwd.clone(),
            project: Some(project),
            session_id: Some(session.session_id.clone()),
            task_id: session.task_id,
            kind: crate::types::NotificationKind::Question,
            level: NotificationLevel::Warn,
            ..NewNotification::new(
                "await",
                title,
                "Claude is waiting for you to choose an option. Press a to answer it from here, \
                 or o to open its terminal.",
            )
        };
    }
    let (title, message) = {
        (
            format!("🔔 {project}: Claude may be waiting on a prompt"),
            "A tool call is waiting, most likely on a permission prompt. Nothing can answer one of those remotely; open its terminal (press o on the session) to clear it.",
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
