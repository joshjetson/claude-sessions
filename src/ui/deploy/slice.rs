//! The Deploy tab's half of the dashboard state.
//!
//! Mirrors [`crate::ui::board::slice`] deliberately: an update arrives from
//! whichever side produced it — the daemon's `deploy` event, or the in-process
//! worker — and everything else is derived. Nothing here performs I/O.

use std::collections::{HashMap, HashSet};

use crate::board::deploy_project_key;
use crate::types::{DeployBoard, DeployProjectState, DeployRun, DeployRunStatus, DeployTask};
use crate::ui::board::BoardDetail;

/// One deploy-board fetch, however it was produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployUpdate {
    pub board: Option<DeployBoard>,
    pub error: Option<String>,
    pub loading: bool,
}

impl DeployUpdate {
    pub fn loading() -> Self {
        DeployUpdate {
            loading: true,
            ..DeployUpdate::default()
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        DeployUpdate {
            error: Some(error.into()),
            ..DeployUpdate::default()
        }
    }

    pub fn loaded(board: DeployBoard) -> Self {
        DeployUpdate {
            board: Some(board),
            ..DeployUpdate::default()
        }
    }
}

/// Everything the Deploy tab draws from.
#[derive(Debug, Default)]
pub struct DeploySlice {
    pub board: Option<DeployBoard>,
    pub error: Option<String>,
    pub loading: bool,
    /// Projects that are open. Survives a refresh — and a refresh is always
    /// something the user asked for here, so it must never collapse the tree
    /// they were reading.
    pub expanded: HashSet<String>,
    /// Deploys the engine is supervising, keyed by project.
    pub runs: HashMap<String, DeployRun>,
    pub detail: Option<BoardDetail>,
    /// Which project's output pane is open, so an arriving line redraws the
    /// pane that is showing it and nothing else.
    pub watching: Option<String>,
}

impl DeploySlice {
    /// Fold an update in.
    pub fn apply(&mut self, update: DeployUpdate) {
        if update.loading && update.board.is_none() && update.error.is_none() {
            self.loading = true;
            return;
        }
        self.loading = false;
        self.error = update.error;
        if let Some(board) = update.board {
            // First load opens every project, so the outstanding work is
            // visible without a keypress — there are only ever a handful.
            if self.expanded.is_empty() {
                for name in &board.project_names {
                    self.expanded.insert(deploy_project_key(name));
                }
            }
            self.board = Some(board);
        }
    }

    /// Replace one run, from the engine's `deploy-run` event.
    pub fn set_run(&mut self, run: DeployRun) {
        self.runs.insert(run.project.clone(), run);
    }

    /// Append a line the engine just captured.
    ///
    /// Only for a run this dashboard already knows about: an output line for a
    /// deploy started before this dashboard connected arrives with no run to
    /// attach it to, and the snapshot is what supplies that.
    pub fn push_line(&mut self, project: &str, line: String) {
        if let Some(run) = self.runs.get_mut(project) {
            run.lines.push(line);
            run.total_lines += 1;
            while run.lines.len() > crate::daemon::WIRE_RUN_LINES {
                run.lines.remove(0);
            }
        }
    }

    pub fn run(&self, project: &str) -> Option<&DeployRun> {
        self.runs.get(project)
    }

    pub fn is_running(&self, project: &str) -> bool {
        self.runs
            .get(project)
            .is_some_and(|run| run.status == DeployRunStatus::Running)
    }

    /// Every deploy actually running, for the shutdown warning. Finished runs
    /// are deliberately not here: warning about one that already exited would
    /// make the warning noise.
    pub fn running_deploys(&self) -> Vec<String> {
        crate::daemon::running_deploys(&self.runs)
    }

    pub fn project(&self, name: &str) -> Option<&DeployProjectState> {
        self.board.as_ref()?.projects.get(name)
    }

    pub fn task(&self, task_id: i64) -> Option<&DeployTask> {
        self.board
            .as_ref()?
            .projects
            .values()
            .flat_map(|project| project.tasks.iter())
            .find(|task| task.id == task_id)
    }

    pub fn toggle(&mut self, key: &str) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_string());
        }
    }

    pub fn set_expanded(&mut self, key: &str, expanded: bool) {
        if expanded {
            self.expanded.insert(key.to_string());
        } else {
            self.expanded.remove(key);
        }
    }
}
