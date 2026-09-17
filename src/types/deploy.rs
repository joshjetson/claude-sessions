//! The Deploy tab's shapes: tasks sitting in the deploy stage, and the live
//! merge-request state overlaid on them.
//!
//! Odoo's cached MR fields go stale (the task's assignee is often not the MR's
//! author, and nobody re-saves the task when an MR merges), so the live record
//! from GitLab is kept separately and wins wherever both exist.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A merge request as GitLab reports it right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    pub iid: i64,
    pub url: String,
    pub title: String,
    /// `opened` | `merged` | `closed` | `locked`.
    pub state: String,
    pub draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    /// Only meaningful once GitLab has finished its mergeability check.
    pub conflicts: bool,
    /// `detailed_merge_status`, falling back to `merge_status`.
    pub merge_status: String,
    /// Head pipeline status: `success` | `failed` | `running` | `pending` | …
    pub pipeline: String,
    /// The MR's author. Routinely not the task's assignee, which is the whole
    /// reason GitLab is read through to rather than trusting Odoo's copy.
    #[serde(default)]
    pub author: String,
}

/// A task on the Deploy tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployTask {
    pub id: i64,
    pub name: String,
    pub state: String,
    /// The human label for `state`, resolved once when the row is built.
    pub state_label: String,
    pub project_name: String,
    pub project_id: i64,
    pub stage_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    pub branch: String,
    pub mr_url: String,
    /// Odoo's cached MR state — replaced by the live one when it lands.
    pub mr_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mr_iid: Option<i64>,
    pub mr_project_path: String,
    /// The live GitLab record, once fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mr: Option<MergeRequest>,
    /// Why the live lookup failed — glab missing, no access, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mr_error: Option<String>,
}

/// One project's column on the Deploy tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployProjectState {
    pub project_id: i64,
    /// No Odoo project with this name — the config names something that is not
    /// there, which is worth saying rather than showing an empty column.
    pub missing: bool,
    /// The configured deploy command; empty means the project cannot deploy.
    pub command: String,
    pub target_branch: String,
    pub tasks: Vec<DeployTask>,
}

/// The Deploy tab as a whole. `configured` is false when no project has opted
/// in, which is its own row rather than an empty tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployBoard {
    pub configured: bool,
    /// Config order, not alphabetical — the order projects were added in.
    pub project_names: Vec<String>,
    pub projects: BTreeMap<String, DeployProjectState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployRunStatus {
    Running,
    Ok,
    Fail,
}

/// A deploy command the dashboard started and the engine is supervising.
///
/// One type for the engine's record and the wire form. The engine keeps up to
/// [`MAX_RUN_LINES`](crate::daemon::MAX_RUN_LINES) of output in a ring buffer
/// and ships the trailing [`WIRE_RUN_LINES`](crate::daemon::WIRE_RUN_LINES)
/// here, with `total_lines` saying how much was cut — a chatty deploy (docker
/// build, gradle) emits tens of thousands of lines and the pane only ever shows
/// a screenful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployRun {
    #[serde(default)]
    pub project: String,
    /// The literal command, so the output pane can show what is running.
    #[serde(default)]
    pub command: String,
    pub status: DeployRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started_at: String,
    /// The trailing window of output.
    #[serde(default)]
    pub lines: Vec<String>,
    /// How many lines the run has produced in total, `lines.len()` included.
    #[serde(default)]
    pub total_lines: usize,
}

impl DeployRun {
    /// A run that has just been started and has produced nothing yet.
    pub fn started(
        project: impl Into<String>,
        command: impl Into<String>,
        started_at: impl Into<String>,
    ) -> DeployRun {
        DeployRun {
            project: project.into(),
            command: command.into(),
            status: DeployRunStatus::Running,
            exit_code: None,
            started_at: started_at.into(),
            lines: Vec::new(),
            total_lines: 0,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status == DeployRunStatus::Running
    }
}
