//! Notifications: raised by the daemon, persisted to SQLite, shown in the board
//! feed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Success,
    Warn,
    Error,
}

impl NotificationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationLevel::Info => "info",
            NotificationLevel::Success => "success",
            NotificationLevel::Warn => "warn",
            NotificationLevel::Error => "error",
        }
    }

    /// Unrecognised text becomes `Info` — the `level` column's default in the
    /// schema, and what the Node app's `n.level || 'info'` produced. A database
    /// written by a future version must never make the feed unreadable.
    pub fn from_label(label: &str) -> Self {
        match label {
            "success" => NotificationLevel::Success,
            "warn" => NotificationLevel::Warn,
            "error" => NotificationLevel::Error,
            _ => NotificationLevel::Info,
        }
    }
}

/// What a notification IS, as distinct from how loudly to announce it.
///
/// `level` answers "how much should this interrupt someone". It never answered
/// the question a QA run has to ask: is the sender blocked, waiting on a reply?
/// An agent stopped at a question is the one thing costing the reviewer time,
/// and a run cannot count those without a field that separates them from
/// progress chatter.
///
/// The set is closed on purpose. An unrecognised label reads as `Info` rather
/// than becoming a new kind, so a typo in a `--kind` flag cannot invent one and
/// cannot accidentally mark something answerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationKind {
    /// Progress, or anything that wants no reply.
    #[default]
    Info,
    /// The sender is blocked and waiting for an answer.
    Question,
    /// A PASS / REVISION REQUIRED decision. Never answered by a coordinator —
    /// see the QA answer policy.
    Verdict,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::Info => "info",
            NotificationKind::Question => "question",
            NotificationKind::Verdict => "verdict",
        }
    }

    /// Anything unrecognised is `Info`. A row written before this column
    /// existed carries the default, which is what those notifications were.
    pub fn from_label(label: &str) -> Self {
        match label {
            "question" => NotificationKind::Question,
            "verdict" => NotificationKind::Verdict,
            _ => NotificationKind::Info,
        }
    }

    /// Whether a coordinating session may answer this on the user's behalf.
    ///
    /// Only an explicit question. A verdict never, and an unknown kind never —
    /// the safe default when you cannot tell what you are answering is not to
    /// answer it.
    pub fn is_answerable(self) -> bool {
        matches!(self, NotificationKind::Question)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationStatus {
    Unread,
    Read,
    Resolved,
    Deleted,
}

impl NotificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationStatus::Unread => "unread",
            NotificationStatus::Read => "read",
            NotificationStatus::Resolved => "resolved",
            NotificationStatus::Deleted => "deleted",
        }
    }

    /// Unrecognised text becomes `Unread`, matching the column default.
    pub fn from_label(label: &str) -> Self {
        match label {
            "read" => NotificationStatus::Read,
            "resolved" => NotificationStatus::Resolved,
            "deleted" => NotificationStatus::Deleted,
            _ => NotificationStatus::Unread,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub message: String,
    pub cwd: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<i64>,
    pub level: NotificationLevel,
    /// What this is, as opposed to how loud it is. Defaults to `Info` so a
    /// sender that predates the field, or omits it, is unchanged.
    #[serde(default)]
    pub kind: NotificationKind,
    /// The QA run whose COORDINATOR sent this, when one did.
    ///
    /// Empty for everything an agent or the dashboard raises. It exists so a
    /// coordinator's own escalation can be told apart from an agent's question:
    /// they are both `Question` notifications about a task in the same run, and
    /// without this the dashboard woke the coordinator with its own message —
    /// which it answered by escalating again, three times in half an hour.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    pub ts: String,
    pub status: NotificationStatus,
}
