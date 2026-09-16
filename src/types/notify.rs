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
    pub ts: String,
    pub status: NotificationStatus,
}
