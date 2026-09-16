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

/// A deploy command the dashboard started and is supervising.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployRun {
    pub status: DeployRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started_at: String,
}
