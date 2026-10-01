//! The things the engine notices on your behalf.
//!
//! - a session waiting on a decision you have not been told about;
//! - a running task session that has gone quiet (not for the QA role);
//! - a task landing in a stage that means it is yours now (for the QA role, a
//!   task landing in a QA stage).
//!
//! The fourth — a session that went away without saying so — is the
//! `vanished` module, because archiving is a different kind of work from
//! raising a notification.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime};

use crate::scan::ProcessSource;
use crate::types::{
    EntryKind, LastEntry, Notification, NotificationKind, NotificationLevel, NotificationStatus,
    Session, SessionStatus,
};
use crate::util::project_name;

use super::alerts::{
    detect_new_assignments, detect_qa_arrivals, detect_stalls, human_duration, last_write,
    StallOptions,
};
use super::engine::EngineInner;
use super::notify::NewNotification;
use super::state::{AwaitWatch, SessionIndex};

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

/// How long a permission prompt must wait before it is announced. The person
/// who is at the keyboard answers most prompts well inside this.
pub const PROMPT_DWELL: Duration = Duration::from_secs(30);

/// How often a prompt that is still waiting is announced again. The old row is
/// deleted first, so a long wait never stacks rows.
pub const PROMPT_REPEAT: Duration = Duration::from_secs(10 * 60);

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
    /// Alert when a session is waiting on YOU: a question it asked, or a
    /// permission prompt.
    ///
    /// Each waiting session has at most one row, tracked in
    /// [`EngineState::awaits`](super::EngineState::awaits):
    ///
    /// - A question is announced at once, and resolved when the session moves
    ///   past it.
    /// - A permission prompt is announced once it has waited [`PROMPT_DWELL`].
    ///   A prompt the person at the keyboard answers at once is not news, and
    ///   one sound per prompt was most of the noise. While it keeps waiting,
    ///   the row is deleted and raised again every [`PROMPT_REPEAT`], so the
    ///   reminder is new without a second row. It is deleted when the prompt
    ///   ends.
    ///
    /// The rows are reconciled against what is waiting now on every tick, not
    /// only on the edges. A session that was killed or went away, and a row
    /// restored from SQLite after a restart, both used to leave "asks you" up
    /// forever, because no edge ever came for them. A restored row whose
    /// session is still waiting is adopted instead of raised again.
    ///
    /// `hooked` names the sessions that have a hook state file. See
    /// [`is_blocked_on_tool_call`]. The refresh loop calls
    /// [`Self::notify_awaiting_decisions_in`], which also takes whether the
    /// scan was complete. This form assumes it was, and exists for the tests.
    #[cfg(test)]
    pub(crate) fn notify_awaiting_decisions(
        &self,
        sessions: &SessionIndex,
        hooked: &HashSet<String>,
        now: SystemTime,
    ) {
        self.notify_awaiting_decisions_in(sessions, hooked, now, true);
    }

    /// The await watcher, told whether the scan behind `sessions` saw
    /// everything it looked for. See `notify_awaiting_decisions` for what it
    /// raises and clears.
    ///
    /// After an incomplete scan, a session missing from `sessions` may still
    /// be there, so no wait is ended for its absence. Ending it would clear
    /// its row, and the next complete scan would raise it again with a sound.
    pub(crate) fn notify_awaiting_decisions_in(
        &self,
        sessions: &SessionIndex,
        hooked: &HashSet<String>,
        now: SystemTime,
        complete: bool,
    ) {
        let rings = self.config().role().notification_policy().prompt_rings();
        let waiting: HashMap<&str, (&Session, bool)> = sessions
            .iter()
            .filter(|session| !session.session_id.is_empty())
            .filter_map(|session| {
                let asked = is_awaiting_user_decision(session.last_entry.as_ref());
                let blocked = !asked
                    && is_blocked_on_tool_call(session, hooked.contains(&session.session_id), now);
                (asked || blocked).then_some((session.session_id.as_str(), (session, asked)))
            })
            .collect();

        let mut ended = EndedRows::default();
        let mut raise: Vec<(String, NewNotification)> = Vec::new();
        {
            let mut guard = self.state();
            let state = &mut *guard;

            let gone: Vec<String> = state
                .awaits
                .keys()
                .filter(|id| !waiting.contains_key(id.as_str()))
                .filter(|id| complete || sessions.get(id).is_some())
                .cloned()
                .collect();
            for id in gone {
                if let Some(watch) = state.awaits.remove(&id) {
                    ended.end(watch);
                }
            }

            for (&id, &(session, asked)) in &waiting {
                if !state.awaits.contains_key(id) {
                    let mut watch = AwaitWatch::new(asked, now);
                    if let Some(row) = restored_row(&state.notifications, id, asked) {
                        watch.row = Some(row);
                        watch.raised_at = Some(now);
                    }
                    state.awaits.insert(id.to_string(), watch);
                }
                let Some(watch) = state.awaits.get_mut(id) else {
                    continue;
                };
                // A prompt that became a question, or the other way round, is
                // a new wait with a different row.
                if watch.asked != asked {
                    ended.end(std::mem::replace(watch, AwaitWatch::new(asked, now)));
                }
                let waited = |since: SystemTime| now.duration_since(since).unwrap_or_default();
                let due = match (&watch.row, asked) {
                    (None, true) => true,
                    (None, false) => waited(watch.since) >= PROMPT_DWELL,
                    (Some(_), true) => false,
                    (Some(_), false) => watch
                        .raised_at
                        .is_none_or(|raised| waited(raised) >= PROMPT_REPEAT),
                };
                if due {
                    if let Some(old) = watch.row.take() {
                        ended.removed.push(old);
                    }
                    raise.push((id.to_string(), awaiting_notification(session, asked, rings)));
                }
            }

            // Rows nothing tracks: restored after a restart for a session that
            // is no longer waiting, or left by a session that went away.
            let tracked: HashSet<&str> = state
                .awaits
                .values()
                .filter_map(|watch| watch.row.as_deref())
                .collect();
            for notification in state.notifications.iter().filter(|_| complete) {
                let orphan = notification.id.starts_with("await-")
                    && notification.status != NotificationStatus::Resolved
                    && !tracked.contains(notification.id.as_str())
                    && !ended.contains(&notification.id);
                if orphan {
                    ended.end_row(notification.id.clone(), notification.kind);
                }
            }
        }

        if !ended.resolved.is_empty() {
            self.set_notification_status(&ended.resolved, NotificationStatus::Resolved);
        }
        if !ended.removed.is_empty() {
            self.dismiss_notifications(&ended.removed);
        }
        for (session_id, notification) in raise {
            let row = self.raise_notification(notification).map(|n| n.id);
            if let Some(watch) = self.state().awaits.get_mut(&session_id) {
                watch.raised_at = row.is_some().then_some(now);
                watch.row = row;
            }
        }
    }

    /// Tell the person when a session stopped on an API error or the usage
    /// limit.
    ///
    /// Claude Code writes the failure into the transcript and ends the turn.
    /// Nothing resumes it, so the session waits, quiet, until someone notices:
    /// that needs the person, and it rings once. The row has a fixed id per
    /// session, so a second failure replaces the first, and it is deleted when
    /// the session moves on or goes away. After a restart, a row already in
    /// the feed is adopted rather than rung again.
    pub(crate) fn notify_api_errors(&self, sessions: &SessionIndex, complete: bool) {
        let mut raise = Vec::new();
        let mut clear = Vec::new();
        {
            let mut guard = self.state();
            let state = &mut *guard;
            for session in sessions.iter() {
                if session.session_id.is_empty() {
                    continue;
                }
                let row = api_error_row(&session.session_id);
                let entry = session.last_entry.as_ref();
                let Some(error) = entry.and_then(|entry| entry.api_error.as_ref()) else {
                    if state.api_errors.remove(&session.session_id).is_some() {
                        clear.push(row);
                    }
                    continue;
                };
                let at = entry
                    .and_then(|entry| entry.activity_at.clone())
                    .unwrap_or_default();
                if state.api_errors.get(&session.session_id) == Some(&at) {
                    continue;
                }
                let restored = !state.api_errors.contains_key(&session.session_id)
                    && state.notifications.iter().any(|n| {
                        n.id == row && n.status != crate::types::NotificationStatus::Resolved
                    });
                state.api_errors.insert(session.session_id.clone(), at);
                if !restored {
                    raise.push(api_error_notification(session, error));
                }
            }
            if complete {
                let gone: Vec<String> = state
                    .api_errors
                    .keys()
                    .filter(|id| sessions.get(id).is_none())
                    .cloned()
                    .collect();
                for id in gone {
                    state.api_errors.remove(&id);
                    clear.push(api_error_row(&id));
                }
            }
        }
        if !clear.is_empty() {
            self.dismiss_notifications(&clear);
        }
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
        // A QA reviewer leaves each session open after its verdict, so a quiet
        // session is the normal end of a pass and not news.
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
    /// It runs only for the QA role, and only when `qa.notifyNewInQa` is on,
    /// because it costs an Odoo query every slow tick. When it has been
    /// skipped, the next tick that runs records the stages silently, exactly
    /// like the first run ever: switching role or the setting must not replay
    /// a week of arrivals.
    pub(crate) fn notify_qa_arrivals(&self) {
        let (policy, enabled, wanted, rule) = {
            let config = self.config();
            (
                config.role().notification_policy(),
                config.alerts().enabled,
                config.qa_notify_new_in_qa(),
                config.qa_alerts(),
            )
        };
        // Switched off, it is paused like a role that does not watch, so
        // switching it on records the stages silently rather than announcing
        // everything that arrived meanwhile.
        if !policy.watches_qa_arrivals() || !wanted {
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

/// The rows of waits that ended this tick.
///
/// A question is resolved, because a QA run reads a resolved question as
/// answered, and the history keeps it. A prompt row is deleted: it was a
/// reminder, and it says nothing once the prompt is gone.
#[derive(Debug, Default)]
struct EndedRows {
    resolved: Vec<String>,
    removed: Vec<String>,
}

impl EndedRows {
    fn end(&mut self, watch: AwaitWatch) {
        if let Some(row) = watch.row {
            if watch.asked {
                self.resolved.push(row);
            } else {
                self.removed.push(row);
            }
        }
    }

    fn end_row(&mut self, row: String, kind: NotificationKind) {
        if kind == NotificationKind::Question {
            self.resolved.push(row);
        } else {
            self.removed.push(row);
        }
    }

    fn contains(&self, row: &str) -> bool {
        self.resolved
            .iter()
            .chain(&self.removed)
            .any(|id| id == row)
    }
}

/// An unresolved `await` row for this session and this kind of wait, as a
/// restart restores them from SQLite.
fn restored_row(
    notifications: &VecDeque<Notification>,
    session_id: &str,
    asked: bool,
) -> Option<String> {
    notifications
        .iter()
        .find(|n| {
            n.id.starts_with("await-")
                && n.status != NotificationStatus::Resolved
                && n.session_id.as_deref() == Some(session_id)
                && (n.kind == NotificationKind::Question) == asked
        })
        .map(|n| n.id.clone())
}

/// The fixed id of a session's API-error row.
fn api_error_row(session_id: &str) -> String {
    format!("apierr-{session_id}")
}

/// The row for a session that stopped on an API error.
fn api_error_notification(session: &Session, error: &crate::types::ApiError) -> NewNotification {
    let project = project_name(&session.cwd);
    let what = match session.task_id {
        Some(task_id) => format!("task #{task_id}"),
        None => "a session".to_string(),
    };
    NewNotification {
        cwd: session.cwd.clone(),
        project: Some(project.clone()),
        session_id: Some(session.session_id.clone()),
        task_id: session.task_id,
        level: NotificationLevel::Error,
        id: Some(api_error_row(&session.session_id)),
        ..NewNotification::new(
            "apierr",
            format!("🛑 {project}: Claude stopped on {what}"),
            format!(
                "{} It will not continue by itself. Open its terminal (o) and tell it to go on.",
                if error.text.is_empty() {
                    error.short()
                } else {
                    error.text.as_str()
                }
            ),
        )
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
///
/// `prompt_rings` is the role's answer to whether a prompt plays a sound. A
/// question always rings.
pub(crate) fn awaiting_notification(
    session: &Session,
    asked: bool,
    prompt_rings: bool,
) -> NewNotification {
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
        silent: !prompt_rings,
        ..NewNotification::new("await", title, message)
    }
}
