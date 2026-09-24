//! Who is using the dashboard, and what that changes.
//!
//! One install serves three kinds of people. A developer starts tasks, answers
//! their agents and fixes revisions. A QA reviewer never starts development
//! work: they pick up tasks that land in a QA stage, run QA passes, and answer
//! the sessions doing those passes. A project manager is a clean slate for now.
//!
//! The role is read from `"role"` in `~/.claude-sessions.json`. Everything that
//! differs by role asks one of the methods below rather than matching on the
//! enum itself, so a later PM feature adds a method here instead of a new
//! `match` in every key handler.

use serde::{Deserialize, Serialize};

use super::{NotificationKind, NotificationLevel};

/// The person at the keyboard.
///
/// Stored as a string in the config file and resolved through
/// [`UserRole::from_label`], so a typo reads as the default instead of failing
/// the parse and losing every other setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// Today's behaviour, unchanged. The default.
    #[default]
    Dev,
    /// A reviewer: QA launches and the answer keys stay, development launches
    /// go, and the notification feed keeps only what a reviewer acts on.
    Qa,
    /// A placeholder that behaves exactly like [`UserRole::Dev`]. It exists so
    /// the config value is accepted now and PM features have a place to land.
    Pm,
}

impl UserRole {
    /// Anything unrecognised is `Dev`, which is the behaviour every install had
    /// before the setting existed. Case and surrounding space are ignored,
    /// because people type this by hand.
    pub fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "qa" => UserRole::Qa,
            "pm" => UserRole::Pm,
            _ => UserRole::Dev,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            UserRole::Dev => "dev",
            UserRole::Qa => "qa",
            UserRole::Pm => "pm",
        }
    }

    /// Whether the board offers the launches that start or resume development
    /// work: start, add context and start, revision, resume the conversation
    /// with `C`, and conflict resolution on the Deploy tab.
    ///
    /// A reviewer who presses `s` by habit starts a development agent on a
    /// task that is in QA. Hiding the keys is cheaper than explaining why that
    /// session exists.
    pub fn shows_dev_actions(self) -> bool {
        !matches!(self, UserRole::Qa)
    }

    /// Which notifications the daemon keeps for this role.
    pub fn notification_policy(self) -> NotificationPolicy {
        match self {
            UserRole::Qa => NotificationPolicy::QaFocused,
            UserRole::Dev | UserRole::Pm => NotificationPolicy::Everything,
        }
    }
}

/// What the daemon stores and announces, decided when a notification is raised.
///
/// Decided at push time rather than at display time, so a dropped notification
/// is never written to SQLite, never plays a sound, and never reaches a client.
/// A filter at display time would still ring for each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationPolicy {
    /// Every source, exactly as before roles existed.
    Everything,
    /// Only what a QA reviewer acts on.
    ///
    /// Kept: a session that needs a decision (`await`), a coordinator or agent
    /// post that is a question or a verdict, any post at warn or error level,
    /// a task that finished or was blocked (`done`, `blocked`), a task that
    /// arrived in a QA stage (`qa-new`), and the one aggregated quiet-sessions
    /// row (`quiet`).
    ///
    /// Dropped: "Approved to Start" arrivals (`assigned`), which are a
    /// developer's queue, the per-task stall alerts (`stalled`), which the
    /// quiet-sessions row replaces, and informational `notify` posts at info or
    /// success level, which are progress chatter from agents.
    QaFocused,
}

impl NotificationPolicy {
    /// Whether a notification from `source` with this kind and level is kept.
    ///
    /// `source` is the id prefix the engine gives each notification. A source
    /// this policy does not name is kept: an unknown source is most likely a
    /// newer alert, and dropping an alert nobody reviewed is the worse error.
    pub fn keeps(self, source: &str, kind: NotificationKind, level: NotificationLevel) -> bool {
        match self {
            NotificationPolicy::Everything => true,
            NotificationPolicy::QaFocused => match source {
                "assigned" | "stalled" => false,
                "notify" => {
                    kind != NotificationKind::Info
                        || matches!(level, NotificationLevel::Warn | NotificationLevel::Error)
                }
                _ => true,
            },
        }
    }

    /// Whether each quiet task session gets its own `stalled` alert. The QA
    /// policy replaces those with one aggregated row for every live session.
    pub fn per_task_stall_alerts(self) -> bool {
        matches!(self, NotificationPolicy::Everything)
    }

    /// Whether the daemon keeps the aggregated quiet-sessions row.
    pub fn quiet_sessions_row(self) -> bool {
        matches!(self, NotificationPolicy::QaFocused)
    }

    /// Whether the daemon watches QA stages for new arrivals.
    pub fn watches_qa_arrivals(self) -> bool {
        matches!(self, NotificationPolicy::QaFocused)
    }
}
