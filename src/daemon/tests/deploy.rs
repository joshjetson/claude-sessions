//! Deploy supervision.
//!
//! - [`runs`] — the ring buffer, the events, and the refusals.
//! - [`complete`] — what a finished run does to Odoo.
//! - [`live`] — the ONE test in this crate that spawns a real deploy.
//!
//! Everything but [`live`] runs under [`SpawnPolicy::Refuse`], so nothing
//! starts a process.

mod complete;
mod live;
mod runs;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::{engine_with, Setup, TestEngine};
use crate::daemon::DeployRunState;
use crate::types::{DeployBoard, DeployProjectState, DeployTask, MergeRequest};

pub(crate) const PROJECT: &str = "Aurora";

pub(crate) fn task(id: i64, mr_state: Option<&str>, state: &str) -> DeployTask {
    DeployTask {
        id,
        name: format!("Task {id}"),
        state: state.to_string(),
        state_label: crate::deploy::state_label(state),
        project_name: PROJECT.to_string(),
        project_id: 3,
        stage_name: "Deployed".to_string(),
        mr: mr_state.map(|state| MergeRequest {
            iid: id,
            state: state.to_string(),
            ..MergeRequest::default()
        }),
        ..DeployTask::default()
    }
}

pub(crate) fn board(tasks: Vec<DeployTask>) -> DeployBoard {
    let mut projects = std::collections::BTreeMap::new();
    projects.insert(
        PROJECT.to_string(),
        DeployProjectState {
            project_id: 3,
            command: "./scripts/deploy-prod.sh".to_string(),
            target_branch: "main".to_string(),
            tasks,
            ..DeployProjectState::default()
        },
    );
    DeployBoard {
        configured: true,
        project_names: vec![PROJECT.to_string()],
        projects,
    }
}

/// An engine whose deploy board is whatever the test says, with a counter so
/// "was it re-read" is answerable.
pub(crate) fn engine_with_board(tasks: Vec<DeployTask>) -> (TestEngine, Arc<AtomicUsize>) {
    let fetches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fetches);
    let board = board(tasks);
    let harness = engine_with(Setup {
        config: Some(json!({
            "deploy": { "projects": { PROJECT: { "command": "./scripts/deploy-prod.sh" } } },
        })),
        deploy: Some(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(board.clone())
        })),
        ..Setup::default()
    });
    (harness, fetches)
}

/// A run in the state, as if one had just been started.
pub(crate) fn seed_run(harness: &TestEngine) {
    harness.state().deploy_runs.insert(
        PROJECT.to_string(),
        DeployRunState::started("./deploy.sh", Some(0)),
    );
}

pub(crate) fn run_lines(harness: &TestEngine) -> Vec<String> {
    harness
        .state()
        .deploy_runs
        .get(PROJECT)
        .map(DeployRunState::log)
        .unwrap_or_default()
}
