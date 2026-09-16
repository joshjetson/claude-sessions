//! The Odoo side: tasks, the board they are grouped into, and the credentials
//! used to fetch them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    pub stage_id: i64,
    pub stage_name: String,
    pub project_id: i64,
    pub project_name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_points: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub child_ids: Vec<i64>,
    #[serde(default)]
    pub subtasks: Vec<Task>,
    /// Odoo task dependencies — ids of tasks that must finish first.
    #[serde(default)]
    pub blocked_by: Vec<i64>,
    #[serde(default)]
    pub blocker_count: u32,
    /// How many blockers are still open. Greater than zero means not ready.
    #[serde(default)]
    pub open_blocker_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardStage {
    pub stage_id: i64,
    pub sequence: i64,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardProject {
    pub project_id: i64,
    /// Keyed by stage name — stage resolution is name-based throughout, never
    /// positional, because Odoo stage ids differ per project.
    pub stages: BTreeMap<String, BoardStage>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub projects: BTreeMap<String, BoardProject>,
    pub task_count: usize,
    #[serde(default)]
    pub truncated: bool,
}

/// Odoo JSON-RPC credentials, resolved from config with the `ODOO_*` env vars
/// as per-field fallbacks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OdooCreds {
    pub url: String,
    pub db: String,
    pub user: String,
    pub password: String,
}

impl OdooCreds {
    /// Node's `hasCredentials()`: all four fields must be non-empty before any
    /// call is attempted, so a half-configured install fails loudly at the board
    /// rather than with an auth error per request.
    pub fn is_complete(&self) -> bool {
        !self.url.is_empty()
            && !self.db.is_empty()
            && !self.user.is_empty()
            && !self.password.is_empty()
    }
}
