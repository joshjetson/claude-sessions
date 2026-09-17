//! The deploy board's rules and its one Odoo query.

mod board;
mod readiness;

use crate::types::{DeployTask, MergeRequest};

/// A task with a linked, loadable merge request — the starting point every
/// readiness test mutates one field of.
pub(crate) fn task_with_mr(mr: MergeRequest) -> DeployTask {
    DeployTask {
        id: 4242,
        name: "Widget rollout".to_string(),
        state: "01_in_progress".to_string(),
        state_label: "In Progress".to_string(),
        project_name: "Aurora".to_string(),
        project_id: 3,
        stage_name: "Deployed".to_string(),
        mr_url: "https://git.example.com/group/repo/-/merge_requests/403".to_string(),
        mr_state: mr.state.clone(),
        mr_iid: Some(mr.iid),
        mr_project_path: "group/repo".to_string(),
        mr: Some(mr),
        ..DeployTask::default()
    }
}

pub(crate) fn open_mr() -> MergeRequest {
    MergeRequest {
        iid: 403,
        url: "https://git.example.com/group/repo/-/merge_requests/403".to_string(),
        title: "Widget rollout".to_string(),
        state: "opened".to_string(),
        source_branch: "task-4242-widget".to_string(),
        target_branch: "development".to_string(),
        merge_status: "mergeable".to_string(),
        pipeline: "success".to_string(),
        ..MergeRequest::default()
    }
}
