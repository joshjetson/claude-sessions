//! The notification feed.
//!
//! Newest first, capped in memory and mirrored into SQLite — the cap keeps a
//! long-running daemon's memory flat, the database is what makes "what did I
//! miss while the dashboard was closed" answerable at all.

use std::sync::atomic::Ordering;

use crate::db::{PRUNE_KEEP, RECENT_LIMIT};
use crate::scan::ProcessSource;
use crate::types::{
    Notification, NotificationKind, NotificationLevel, NotificationStatus, QUIET_SESSIONS_ID,
};
use crate::util::{iso_now, project_name};

use super::engine::EngineInner;
use super::events::EngineEvent;
use super::lock;

/// How many notifications the feed holds in memory. Mandate #14: a
/// [`std::collections::VecDeque`], so pushing the 201st is a pop rather than
/// the `unshift` + `truncate` that reallocated the whole list every time.
pub const NOTIFICATION_LIMIT: usize = 200;

/// What a caller knows; the engine fills in the id, the timestamp and the
/// project name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNotification {
    /// Prefix for the generated id, so the feed reads as what raised it
    /// (`await`, `stalled`, `assigned`, `done`, `blocked`, `notify`).
    ///
    /// Named `source` rather than `kind` because [`NotificationKind`] now means
    /// something else entirely — whether the sender is blocked waiting for a
    /// reply. Two fields called `kind` in one struct, meaning an id prefix and
    /// an answerability class, is a mistake waiting to be made at a call site.
    pub source: &'static str,
    /// What this is: progress, a question someone is blocked on, or a verdict.
    pub kind: NotificationKind,
    pub title: String,
    pub message: String,
    pub cwd: String,
    /// Left unset, it is derived from `cwd` — which is what every caller but
    /// the board watcher wants.
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<i64>,
    /// Set only when a run's COORDINATOR raised this, so the dashboard can tell
    /// its escalation apart from an agent's question and not wake it with its
    /// own message.
    pub run_id: String,
    pub level: NotificationLevel,
}

impl NewNotification {
    pub fn new(source: &'static str, title: impl Into<String>, message: impl Into<String>) -> Self {
        NewNotification {
            source,
            kind: NotificationKind::Info,
            title: title.into(),
            message: message.into(),
            cwd: String::new(),
            project: None,
            run_id: String::new(),
            session_id: None,
            task_id: None,
            level: NotificationLevel::Info,
        }
    }
}

/// The answer to an action a client asked for. Mirrors the Node `{ ok, error }`
/// the HTTP layer turns into 202 or 400.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub ok: bool,
    pub error: Option<String>,
}

impl ActionResult {
    pub fn ok() -> Self {
        ActionResult {
            ok: true,
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        ActionResult {
            ok: false,
            error: Some(error.into()),
        }
    }
}

impl<S: ProcessSource> EngineInner<S> {
    /// Raise a notification, unless the user's role does not want it.
    ///
    /// Every alert the daemon raises on its own comes through here, and so
    /// does `POST /notify`. The role is read when the notification is raised,
    /// so a dropped one is never stored, never sent and never rings. The role
    /// comes from the engine's config, which each tick re-reads when the file
    /// changes, so editing `"role"` takes effect within a second.
    ///
    /// `None` means the policy dropped it.
    pub(crate) fn raise_notification(&self, new: NewNotification) -> Option<Notification> {
        let policy = self.config().role().notification_policy();
        if !policy.keeps(new.source, new.kind, new.level) {
            return None;
        }
        Some(self.push_notification(new))
    }

    /// Raise a notification: into the feed, into SQLite, out to every client.
    ///
    /// No role check. Production alerts go through [`Self::raise_notification`].
    pub(crate) fn push_notification(&self, new: NewNotification) -> Notification {
        let project = new.project.unwrap_or_else(|| {
            if new.cwd.is_empty() {
                String::new()
            } else {
                project_name(&new.cwd)
            }
        });
        let notification = Notification {
            id: self.notification_id(new.source),
            title: new.title,
            message: new.message,
            cwd: new.cwd,
            project,
            session_id: new.session_id,
            task_id: new.task_id,
            level: new.level,
            kind: new.kind,
            run_id: new.run_id,
            ts: iso_now(),
            status: NotificationStatus::Unread,
        };

        {
            let mut state = lock(&self.state);
            state.notifications.push_front(notification.clone());
            state.notifications.truncate(NOTIFICATION_LIMIT);
        }
        self.db.put_notification(&notification);
        self.publish(EngineEvent::Notification(Box::new(notification.clone())));
        notification
    }

    /// Ids are unique within a process even when two are raised in the same
    /// millisecond — Node used a bare `Date.now()` for the completion
    /// notification, which two tasks finishing together would collide on.
    fn notification_id(&self, source: &str) -> String {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{source}-{}-{seq}", chrono::Utc::now().timestamp_millis())
    }

    /// Change the status of notifications (read / resolved).
    ///
    /// The daemon owns the list, so a client marking one resolved has to say so
    /// here — otherwise its next snapshot overwrites the change. And it has to
    /// reach SQLite, or the status comes back on the next restart.
    pub(crate) fn set_notification_status(
        &self,
        ids: &[String],
        status: NotificationStatus,
    ) -> ActionResult {
        if ids.is_empty() {
            return ActionResult::failed("no ids");
        }
        {
            let mut state = lock(&self.state);
            for notification in state.notifications.iter_mut() {
                if ids.contains(&notification.id) {
                    notification.status = status;
                }
            }
            // Reading the quiet row is looking at it. Resolving it is saying
            // "I know", which is a dismissal.
            if status != NotificationStatus::Unread
                && status != NotificationStatus::Read
                && ids.iter().any(|id| id == QUIET_SESSIONS_ID)
            {
                state.quiet.dismiss();
            }
        }
        self.db.set_notification_status(ids, status);
        self.publish(EngineEvent::NotificationsChanged {
            ids: ids.to_vec(),
            status: Some(status),
            removed: false,
        });
        ActionResult::ok()
    }

    /// Remove notifications entirely.
    pub(crate) fn dismiss_notifications(&self, ids: &[String]) -> ActionResult {
        if ids.is_empty() {
            return ActionResult::failed("no ids");
        }
        {
            let mut state = lock(&self.state);
            state
                .notifications
                .retain(|notification| !ids.contains(&notification.id));
            if ids.iter().any(|id| id == QUIET_SESSIONS_ID) {
                state.quiet.dismiss();
            }
        }
        self.db.delete_notifications(ids);
        self.publish(EngineEvent::NotificationsChanged {
            ids: ids.to_vec(),
            status: None,
            removed: true,
        });
        ActionResult::ok()
    }

    /// Mark the named notifications read, or the whole feed when none are
    /// named — the "mark all read" key in the board view.
    pub(crate) fn mark_notifications_read(&self, ids: &[String]) -> ActionResult {
        if !ids.is_empty() {
            return self.set_notification_status(ids, NotificationStatus::Read);
        }
        let all: Vec<String> = {
            let mut state = lock(&self.state);
            for notification in state.notifications.iter_mut() {
                notification.status = NotificationStatus::Read;
            }
            state.notifications.iter().map(|n| n.id.clone()).collect()
        };
        self.db.mark_notifications_read::<String>(&[]);
        self.publish(EngineEvent::NotificationsChanged {
            ids: all,
            status: Some(NotificationStatus::Read),
            removed: false,
        });
        ActionResult::ok()
    }

    /// Resolve every notification in the feed: the Clear all key.
    ///
    /// Resolved rather than deleted. Resolved is the status every view already
    /// hides, the "asks you" markers ignore, and a QA run reads as answered, so
    /// one status change clears all of them. The rows stay in SQLite as the
    /// history of what was raised, and the table is pruned as before. SQLite is
    /// updated as a whole, not only the rows held in memory, so an older unread
    /// row cannot come back after a restart.
    pub(crate) fn resolve_all_notifications(&self) -> ActionResult {
        let ids: Vec<String> = {
            let mut state = lock(&self.state);
            let mut ids = Vec::new();
            for notification in state.notifications.iter_mut() {
                if notification.status != NotificationStatus::Resolved {
                    notification.status = NotificationStatus::Resolved;
                    ids.push(notification.id.clone());
                }
            }
            if state.quiet.shown {
                state.quiet.dismiss();
            }
            ids
        };
        self.db.resolve_all_notifications();
        if !ids.is_empty() {
            self.publish(EngineEvent::NotificationsChanged {
                ids,
                status: Some(NotificationStatus::Resolved),
                removed: false,
            });
        }
        ActionResult::ok()
    }

    /// Resolve the questions a session asked, once it stops waiting.
    ///
    /// The `await` watcher raises a `Question` when a session stops on a
    /// decision. When the session moves on, somebody answered it, in its own
    /// terminal or from here. Without this the question stayed unresolved, and
    /// the board kept drawing "← asks you" beside a session that was working.
    pub(crate) fn resolve_answered_questions(&self, session_ids: &[String]) {
        if session_ids.is_empty() {
            return;
        }
        let ids: Vec<String> = lock(&self.state)
            .notifications
            .iter()
            .filter(|n| {
                n.kind == NotificationKind::Question
                    && n.status != NotificationStatus::Resolved
                    && n.id.starts_with("await-")
                    && n.session_id
                        .as_ref()
                        .is_some_and(|id| session_ids.contains(id))
            })
            .map(|n| n.id.clone())
            .collect();
        if !ids.is_empty() {
            self.set_notification_status(&ids, NotificationStatus::Resolved);
        }
    }

    /// Put the quiet-sessions row in the feed with its fixed id, and ring.
    ///
    /// Any older copy is dropped first, so a row the user resolved earlier
    /// cannot sit beside the new one. It is not written to SQLite: see
    /// [`QUIET_SESSIONS_ID`].
    pub(crate) fn raise_quiet_row(&self, title: String, message: String) {
        let notification = quiet_row(title, message);
        {
            let mut state = lock(&self.state);
            state.notifications.retain(|n| n.id != QUIET_SESSIONS_ID);
            state.notifications.push_front(notification.clone());
            state.notifications.truncate(NOTIFICATION_LIMIT);
        }
        self.publish(EngineEvent::Notification(Box::new(notification)));
    }

    /// Refresh the quiet-sessions row's text in place, without a sound.
    ///
    /// Published only when the text changed, which is about once a minute
    /// while the durations tick up. A row that fell off the end of the capped
    /// list is put back at the front.
    pub(crate) fn update_quiet_row(&self, title: String, message: String) {
        let updated = {
            let mut state = lock(&self.state);
            match state
                .notifications
                .iter_mut()
                .find(|n| n.id == QUIET_SESSIONS_ID)
            {
                Some(row) if row.title == title && row.message == message => None,
                Some(row) => {
                    row.title = title;
                    row.message = message;
                    Some(row.clone())
                }
                None => {
                    let row = quiet_row(title, message);
                    state.notifications.push_front(row.clone());
                    state.notifications.truncate(NOTIFICATION_LIMIT);
                    Some(row)
                }
            }
        };
        if let Some(row) = updated {
            self.publish(EngineEvent::NotificationUpdated(Box::new(row)));
        }
    }

    /// Take the quiet-sessions row out of the feed.
    pub(crate) fn remove_quiet_row(&self) {
        let removed = {
            let mut state = lock(&self.state);
            let before = state.notifications.len();
            state.notifications.retain(|n| n.id != QUIET_SESSIONS_ID);
            state.notifications.len() != before
        };
        if removed {
            self.publish(EngineEvent::NotificationsChanged {
                ids: vec![QUIET_SESSIONS_ID.to_string()],
                status: None,
                removed: true,
            });
        }
    }

    /// Reload the feed at startup and trim the table behind it.
    ///
    /// The feed now survives a restart — previously anything that arrived while
    /// the dashboard was down was lost entirely.
    pub(crate) fn restore_notifications(&self) {
        let restored = self.db.recent_notifications(RECENT_LIMIT);
        if !restored.is_empty() {
            let mut state = lock(&self.state);
            state.notifications = restored.into_iter().take(NOTIFICATION_LIMIT).collect();
        }
        self.db.prune_notifications(PRUNE_KEEP);
    }
}

/// The quiet-sessions row. Warn level, so it rings the warn sound once when it
/// is raised, and `Info` kind, because nothing in it can be answered.
fn quiet_row(title: String, message: String) -> Notification {
    Notification {
        id: QUIET_SESSIONS_ID.to_string(),
        title,
        message,
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: None,
        level: NotificationLevel::Warn,
        kind: NotificationKind::Info,
        run_id: String::new(),
        ts: iso_now(),
        status: NotificationStatus::Unread,
    }
}
