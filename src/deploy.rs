//! The Deploy tab's data.
//!
//! It answers one question per project: *what is parked in the Deployed stage
//! that is not actually finished, and what has to be merged before I can ship?*
//!
//! Only projects with an entry under `deploy.projects` in
//! `~/.claude-sessions.json` appear — the tab is opt-in, because it is the one
//! place in the app that runs a production command.
//!
//! [`fetch`] loads the board and overlays live GitLab state; this file holds
//! the rules, which are pure and therefore the part worth pinning: what counts
//! as finished, and exactly why a merge request cannot be merged right now.

mod fetch;

pub use fetch::{
    deploy_specs, enrich_with_live_mrs, fetch_deploy_board, to_deploy_task, DeployProjectSpec,
    DEPLOY_TASK_FIELDS,
};

use crate::odoo::task_state;
use crate::types::{DeployTask, MergeRequest};

/// The stage a task sits in once it has shipped to a non-production
/// environment. Name-based like every other stage lookup in this crate: stage
/// ids differ per project.
pub const DEPLOY_STAGE: &str = "Deployed";

/// `project.task` states that mean the task is finished and needs nothing
/// further. Everything else in the Deployed stage is outstanding work.
pub const FINISHED_STATES: [&str; 3] = [
    task_state::COMPLETE,
    task_state::DONE,
    task_state::CANCELLED,
];

/// The human label for a raw Odoo state key.
///
/// `03_approved` is "Complete", NOT "Done" — they are different states and the
/// deploy flow only ever sets the first one.
pub fn state_label(state: &str) -> String {
    match state {
        task_state::IN_PROGRESS => "In Progress",
        task_state::CHANGES_REQUESTED => "Changes Requested",
        task_state::COMPLETE => "Complete",
        task_state::WAITING => "Waiting",
        task_state::DONE => "Done",
        task_state::CANCELLED => "Cancelled",
        "" => "Unknown",
        other => other,
    }
    .to_string()
}

/// Whether a task's merge request can be merged right now, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// Empty when ready. Otherwise the SPECIFIC blocker, because "cannot merge"
    /// on its own sends the user to GitLab to find out what this already knows.
    pub reason: String,
}

impl Readiness {
    fn ready() -> Readiness {
        Readiness {
            ready: true,
            reason: String::new(),
        }
    }

    fn blocked(reason: impl Into<String>) -> Readiness {
        Readiness {
            ready: false,
            reason: reason.into(),
        }
    }
}

/// Is this task's merge request ready to merge?
///
/// The order is the Node original's and it matters: the reasons are checked
/// from "there is nothing to merge" outwards to "GitLab says no", so the most
/// fundamental problem is the one reported.
pub fn merge_readiness(task: &DeployTask) -> Readiness {
    if task.mr_url.is_empty() && task.mr_iid.is_none() {
        return Readiness::blocked("no merge request linked");
    }
    if task.mr_project_path.is_empty() {
        return Readiness::blocked("MR linked but its GitLab project is unknown");
    }
    let Some(mr) = &task.mr else {
        return match &task.mr_error {
            Some(error) => Readiness::blocked(format!("MR status unavailable ({error})")),
            None => Readiness::blocked("MR status not loaded yet"),
        };
    };
    if mr.state == "merged" {
        return Readiness::blocked("already merged");
    }
    if mr.state == "closed" {
        return Readiness::blocked("MR is closed");
    }
    if mr.draft {
        return Readiness::blocked("MR is a draft");
    }
    if mr.conflicts {
        return Readiness::blocked("has merge conflicts");
    }
    // Anything GitLab itself will not merge, in GitLab's own words — it knows
    // about approval rules and pipeline policies that this crate does not.
    if !mr.merge_status.is_empty() && mr.merge_status != "mergeable" {
        return Readiness::blocked(format!(
            "GitLab says: {}",
            mr.merge_status.replace('_', " ")
        ));
    }
    Readiness::ready()
}

/// Is this task blocked specifically by merge conflicts — the one merge blocker
/// an agent can actually clear?
pub fn has_conflicts(task: &DeployTask) -> bool {
    let Some(mr) = &task.mr else {
        return false;
    };
    // Only an OPEN merge request can be un-conflicted; a closed one's stale
    // conflict flag is not work for anybody.
    mr.state == "opened" && (mr.conflicts || mr.merge_status.to_lowercase().contains("conflict"))
}

/// Whether the code behind this task is actually in the branch a deploy builds.
///
/// "Shipped" means MERGED, and nothing weaker: a task whose MR is still open,
/// conflicted or missing did not ship, however far along the board it looks.
pub fn has_shipped(task: &DeployTask) -> bool {
    task.mr
        .as_ref()
        .is_some_and(|mr: &MergeRequest| mr.state == "merged")
}

#[cfg(test)]
mod tests;
