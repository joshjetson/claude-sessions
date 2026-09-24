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
    /// Odoo's `state`, as written: `03_approved` is "Complete", `1_done` is
    /// "Done". Optional and skipped when absent, so a board from a daemon that
    /// predates the field, or sent to a client that does, still parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A badge for an Odoo `state` worth seeing on a task that sits in a QA stage.
///
/// Odoo moves `state` and the stage independently. A developer sets Complete
/// when the work is done, or a reviewer sets Changes Requested, and nothing
/// moves the stage, so the QA Board once found tasks left in QA that way for
/// 195 days. The badge shows what Odoo says without anyone opening the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QaStateBadge {
    /// What the board row shows.
    pub glyph: &'static str,
    /// The line the detail pane shows.
    pub detail: &'static str,
    /// Drawn in the warning colour rather than the success colour.
    pub attention: bool,
}

/// The badge for a task's Odoo `state`, or `None`.
///
/// One mapping for every state, so adding one is adding an arm here. Only a
/// task in a QA stage gets a badge: elsewhere these states are the normal end
/// of a task's life and say nothing new.
pub fn qa_state_badge(state: Option<&str>, in_qa_stage: bool) -> Option<QaStateBadge> {
    use crate::odoo::task_state::{CHANGES_REQUESTED, COMPLETE};
    if !in_qa_stage {
        return None;
    }
    match state? {
        COMPLETE => Some(QaStateBadge {
            glyph: "✅",
            detail: "✅ Marked Complete in Odoo",
            attention: false,
        }),
        CHANGES_REQUESTED => Some(QaStateBadge {
            glyph: "🔁",
            detail: "🔁 Changes Requested in Odoo",
            attention: true,
        }),
        _ => None,
    }
}

impl Task {
    /// Whether the task sits in one of these stages. Stage names match
    /// case-insensitively and trimmed, like every other stage comparison,
    /// because people type them into config by hand.
    pub fn in_stage(&self, stages: &[String]) -> bool {
        let stage = self.stage_name.trim().to_lowercase();
        stages
            .iter()
            .any(|name| name.trim().to_lowercase() == stage)
    }

    /// This task's QA-stage badge, given the stages that mean "waiting on QA".
    pub fn qa_state_badge(&self, qa_stages: &[String]) -> Option<QaStateBadge> {
        qa_state_badge(self.state.as_deref(), self.in_stage(qa_stages))
    }
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
