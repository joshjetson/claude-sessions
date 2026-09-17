//! What a finished deploy does: close out the tasks that actually shipped, and
//! reload the board it decided that from.
//!
//! Split out of the supervisor because the two halves answer different
//! questions — "is the process still running" and "what did it ship" — and the
//! second is the one with the rule worth reading on its own.

use crate::deploy::has_shipped;
use crate::odoo::task_state;
use crate::scan::ProcessSource;
use crate::types::{DeployBoard, DeployRunStatus};
use crate::util::truncate;

use super::super::engine::EngineInner;
use super::super::events::EngineEvent;

impl<S: ProcessSource + Send + 'static> EngineInner<S> {
    pub(crate) fn finish_deploy(&self, project: &str, code: Option<i32>) {
        {
            let mut state = self.state();
            let Some(run) = state.deploy_runs.get_mut(project) else {
                return;
            };
            run.exit_code = code;
            run.status = if code == Some(0) {
                DeployRunStatus::Ok
            } else {
                DeployRunStatus::Fail
            };
            run.pid = None;
        }
        self.publish_run(project);

        // Only a clean deploy closes tasks out — a failed one shipped nothing.
        if code != Some(0) {
            return;
        }
        // Re-read the board first: "did this task ship" is answered by GitLab,
        // and a board loaded before the deploy started predates every merge the
        // run depended on.
        self.poll_deploy();
        self.complete_shipped_tasks(project);
        self.publish_run(project);
    }

    /// Mark the tasks whose merge requests actually merged Complete in Odoo.
    ///
    /// Sets `03_approved` — "Complete", never "Done" — and NEVER touches the
    /// stage: the task stays in Deployed, which is where it belongs until a
    /// human says otherwise. Tasks whose MR never merged are deliberately left
    /// alone and reported, because they did not ship.
    fn complete_shipped_tasks(&self, project: &str) {
        let tasks = {
            let state = self.state();
            let Some(board) = &state.deploy else {
                return;
            };
            let Some(entry) = board.projects.get(project) else {
                return;
            };
            entry.tasks.clone()
        };

        let (shipped, skipped): (Vec<_>, Vec<_>) = tasks.into_iter().partition(has_shipped);
        let shipped: Vec<_> = shipped
            .into_iter()
            .filter(|task| task.state != task_state::COMPLETE)
            .collect();

        if shipped.is_empty() {
            if !skipped.is_empty() {
                self.report(
                    project,
                    format!(
                        "— {} task(s) left as-is: their MRs aren't merged, so they didn't ship.",
                        skipped.len()
                    ),
                );
            }
            return;
        }

        self.report(project, String::new());
        self.report(
            project,
            format!(
                "— Marking {} shipped task(s) Complete in Odoo…",
                shipped.len()
            ),
        );
        for task in &shipped {
            let name = truncate(&task.name, 46);
            match self.backend.set_task_state(task.id, task_state::COMPLETE) {
                Ok(()) => self.report(project, format!("   ✓ #{} {name} → Complete", task.id)),
                Err(error) => self.report(project, format!("   ✗ #{} failed: {error}", task.id)),
            }
        }
        for task in &skipped {
            // The MR's own state, not the task's: "opened" is the reason it
            // did not ship, and the task's state says nothing about that.
            let why = match &task.mr {
                Some(mr) => mr.state.clone(),
                None => "no MR".to_string(),
            };
            self.report(
                project,
                format!("   – #{} left alone ({why} — not shipped)", task.id),
            );
        }
    }

    /// One deploy-board fetch, best-effort. Shared by the manual refresh and
    /// the post-deploy re-read so "a failed fetch keeps the previous board" is
    /// written once.
    pub(crate) fn poll_deploy(&self) {
        let Some(fetch) = &self.fetch_deploy else {
            return;
        };
        let error = match fetch() {
            Ok(board) => {
                self.set_deploy_board(Some(board), None);
                None
            }
            Err(error) => {
                // The previous board stays: a stale list with an error beside
                // it beats an empty tab.
                self.set_deploy_board(None, Some(error.clone()));
                Some(error)
            }
        };
        self.publish(EngineEvent::Deploy { error });
    }

    fn set_deploy_board(&self, board: Option<DeployBoard>, error: Option<String>) {
        let mut state = self.state();
        if board.is_some() {
            state.deploy = board;
        }
        state.deploy_error = error;
    }
}
