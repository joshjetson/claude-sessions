//! What the completion and launch flows need from the outside world, and the
//! Odoo implementation of it.
//!
//! The contract lives with its one real implementation rather than with the
//! flow that calls it, so "what can be asked" and "how it is answered" are read
//! together. [`super::completion`] owns only the ORDER.
//!
//! [`super::completion`] owns the ORDER things happen in when a task finishes;
//! this owns the four round trips it makes. Both the daemon's completion flow
//! and the dashboard's launch flow go through the same object, so there is one
//! implementation of "resolve a stage by name and move the task into it" rather
//! than the Node app's two (`processDone` in the engine, `moveTaskToInProgress`
//! in the TUI, with subtly different fallbacks).
//!
//! Nothing here guesses. A stage the project does not have leaves the task
//! exactly where it is and says which name was looked for — the Node original's
//! one hard rule, learned from a finished task landing in "Revision Required"
//! because something walked one column forward.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::ConfigHandle;
use crate::gitlab::{can_open_mr_from, Gitlab, FALLBACK_TARGET};
use crate::odoo::{OdooClient, StageKind};
use crate::pipeline::dashboard_step;

/// What Odoo knows about a task the board has not loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetail {
    pub project_id: Option<i64>,
    pub stage_id: Option<i64>,
    pub name: String,
    pub project_name: String,
}

/// A finished task that may need a merge request opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequestRequest {
    pub task_id: i64,
    /// Where the work was done — the repository `glab` is run in.
    pub cwd: PathBuf,
    pub project_id: Option<i64>,
    pub project_name: String,
}

/// Where a merge request opened by the safety net should land.
///
/// The order is the Node original's and each step is a fallback for the one
/// before: the user's explicit `targetBranches` entry, then whatever the Odoo
/// project's linked repository calls its default branch, then `development`.
pub fn resolve_target_branch(configured: Option<&str>, project_default: Option<String>) -> String {
    configured
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .or(project_default)
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| FALLBACK_TARGET.to_string())
}

/// A task that may need moving to another stage.
///
/// One request type for both directions. The Node app had `resolveDoneStage`
/// and `resolveInProgressStage` with identical bodies and different constant
/// lists, and `moveTaskToInProgress` in the TUI duplicating the whole
/// resolve-then-write dance that `processDone` already did on the daemon side.
/// Here [`StageKind`] carries the difference and there is one implementation
/// (WORKING.md rule 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageMoveRequest {
    pub task_id: i64,
    /// Which end of the task's life this move is: QA-ward or work-ward.
    pub kind: StageKind,
    /// The pipeline whose dashboard step decides whether the move happens at
    /// all — `task`/`revision` for a start, `task` for a completion.
    pub pipeline_id: String,
    /// The dashboard step within it: `move-in-progress` or `move-qa`.
    pub step_id: String,
    pub project_id: Option<i64>,
    /// The stage it is in now, which the resolver uses to avoid moving
    /// backwards.
    pub stage_id: Option<i64>,
    pub project_name: String,
    /// The repository, so the project's own pipeline definition is the one
    /// consulted: the flow shown under `P` is the flow that runs.
    pub repo_path: Option<PathBuf>,
    /// The configured stage-name override, tried before the built-in list.
    pub preferred: Vec<String>,
}

impl StageMoveRequest {
    /// The completion flow's move: `task` pipeline, `move-qa` step, QA-ward.
    pub fn to_done(task_id: i64) -> Self {
        StageMoveRequest {
            task_id,
            kind: StageKind::Done,
            pipeline_id: "task".to_string(),
            step_id: MOVE_QA_STEP.to_string(),
            project_id: None,
            stage_id: None,
            project_name: String::new(),
            repo_path: None,
            preferred: Vec::new(),
        }
    }

    /// The launch flow's move: work-ward, from whichever pipeline started it.
    pub fn to_in_progress(task_id: i64, pipeline_id: impl Into<String>) -> Self {
        StageMoveRequest {
            kind: StageKind::InProgress,
            pipeline_id: pipeline_id.into(),
            step_id: MOVE_IN_PROGRESS_STEP.to_string(),
            ..StageMoveRequest::to_done(task_id)
        }
    }
}

/// The dashboard steps that decide whether a stage move happens. Named once:
/// a typo in either string silently disables the move.
pub const MOVE_QA_STEP: &str = "move-qa";
pub const MOVE_IN_PROGRESS_STEP: &str = "move-in-progress";

/// What became of a stage move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageMove {
    /// The project's pipeline has no enabled stage-move step — moving the task
    /// is not this flow's job, and that is not an error.
    Disabled,
    Moved(String),
    /// Already in the target stage, so nothing was written. Distinct from
    /// `Moved`: the notification must not claim a move that did not happen.
    Unchanged(String),
    /// Nothing matched. Never guess forward: the next stage along could be
    /// "Revision Required".
    NoStage(String),
}

/// Everything the completion flow needs from the outside world.
///
/// Phase 9b implements the Odoo half (`task_detail`, `move_to_done_stage`,
/// `post_comment`), Phase 10 the GitLab half (`ensure_merge_request`). Each
/// returns `Result<_, String>` because the message is shown to the user in the
/// completion notification, not logged.
pub trait TaskBackend: Send + Sync {
    /// `project.task` read, for a task the board has not loaded.
    fn task_detail(&self, task_id: i64) -> Result<TaskDetail, String>;

    /// Safety net: if a finished task has no merge request yet, open one.
    ///
    /// The implementation is expected to skip when Odoo already holds an MR
    /// URL, and when the branch is `development`/`main`/`master` or detached —
    /// there is nothing to open a merge request from.
    fn ensure_merge_request(&self, request: &MergeRequestRequest)
        -> Result<Option<String>, String>;

    /// Resolve the target stage by NAME and move the task there.
    ///
    /// Both the completion flow (QA-ward) and the launch flow (work-ward) call
    /// this; [`StageMoveRequest::kind`] is the only difference between them.
    fn move_to_stage(&self, request: &StageMoveRequest) -> Result<StageMove, String>;

    /// Post the completion comment on the task's chatter, as HTML.
    fn post_comment(&self, task_id: i64, html: &str) -> Result<(), String>;

    /// Write a task's `state` — the axis the stage is NOT.
    ///
    /// The deploy flow's only Odoo write: a task whose merge request shipped
    /// becomes `03_approved` ("Complete"), and its stage is left exactly where
    /// it is.
    fn set_task_state(&self, task_id: i64, state: &str) -> Result<(), String>;
}

/// The backend until a later phase installs a real one: every remote step is a
/// no-op, and says so.
///
/// Not a panic and not an error the flow aborts on — a daemon with no Odoo
/// credentials must still archive transcripts and raise notifications.
pub struct NullBackend;

impl TaskBackend for NullBackend {
    fn task_detail(&self, _task_id: i64) -> Result<TaskDetail, String> {
        Ok(TaskDetail::default())
    }

    fn ensure_merge_request(
        &self,
        _request: &MergeRequestRequest,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn move_to_stage(&self, _request: &StageMoveRequest) -> Result<StageMove, String> {
        Ok(StageMove::Disabled)
    }

    fn post_comment(&self, _task_id: i64, _html: &str) -> Result<(), String> {
        Ok(())
    }

    fn set_task_state(&self, _task_id: i64, _state: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Everything the completion and launch flows need from Odoo.
///
/// Holds the client rather than credentials: the client owns the uid and the
/// metadata caches, and sharing one means a stage move does not re-authenticate
/// behind a board refresh.
pub struct OdooTaskBackend {
    client: Arc<OdooClient>,
    gitlab: Gitlab,
    /// Only the `targetBranches` map is read out of it, on the one path that
    /// opens a merge request. Held rather than copied so an edit through the
    /// dialog takes effect without a restart.
    config: ConfigHandle,
}

impl OdooTaskBackend {
    pub fn new(client: Arc<OdooClient>, gitlab: Gitlab, config: ConfigHandle) -> Self {
        OdooTaskBackend {
            client,
            gitlab,
            config,
        }
    }

    pub fn client(&self) -> &Arc<OdooClient> {
        &self.client
    }

    pub fn gitlab(&self) -> &Gitlab {
        &self.gitlab
    }
}

impl TaskBackend for OdooTaskBackend {
    fn task_detail(&self, task_id: i64) -> Result<TaskDetail, String> {
        let detail = self
            .client
            .get_task_detail(task_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("Odoo has no task #{task_id}"))?;
        // One read answers both halves: `to_detail` keeps the ids off the same
        // record the names came from, so no second round trip is needed.
        Ok(TaskDetail {
            project_id: Some(detail.project_id).filter(|id| *id != 0),
            stage_id: Some(detail.stage_id).filter(|id| *id != 0),
            name: detail.name,
            project_name: detail.project_name,
        })
    }

    /// The safety net: a finished task with no merge request gets one.
    ///
    /// Three skips, and each one is the difference between a useful safety net
    /// and a machine that opens junk merge requests:
    ///
    /// * Odoo already holds a URL — the MR exists, and a second one against the
    ///   same branch is noise somebody has to close.
    /// * The branch is `development`, `main` or `master` — those are where
    ///   merge requests LAND. Opening one from them means something upstream
    ///   went wrong, and a merge request would not fix it.
    /// * Detached HEAD — there is no branch to push, so there is nothing to
    ///   open a merge request from.
    ///
    /// Not finding a merge request is a normal outcome, not an error: the
    /// completion notification says "no merge request detected", which is true.
    fn ensure_merge_request(
        &self,
        request: &MergeRequestRequest,
    ) -> Result<Option<String>, String> {
        if request.cwd.as_os_str().is_empty() {
            return Ok(None);
        }
        if let Some(gitlab) = self.client.get_task_gitlab(request.task_id) {
            if !gitlab.merge_request_url.is_empty() {
                return Ok(Some(gitlab.merge_request_url));
            }
        }
        let Some(branch) = self.gitlab.current_branch(&request.cwd) else {
            return Ok(None);
        };
        if !can_open_mr_from(&branch) {
            return Ok(None);
        }
        let target = resolve_target_branch(
            self.config.target_branch(&request.project_name),
            request
                .project_id
                .and_then(|id| self.client.get_project_default_branch(id)),
        );
        self.gitlab
            .create_mr(&request.cwd, &branch, &target)
            .map_err(|err| err.to_string())
    }

    fn move_to_stage(&self, request: &StageMoveRequest) -> Result<StageMove, String> {
        // What the project's own pipeline says about this move, so the flow
        // shown under `P` is the flow that runs: skip the step and the move
        // stops happening; set its `stage` and it goes somewhere else.
        let step = dashboard_step(
            &request.pipeline_id,
            request.repo_path.as_deref(),
            &request.step_id,
        );
        if !step.enabled {
            return Ok(StageMove::Disabled);
        }
        let Some(project_id) = request.project_id else {
            return Ok(StageMove::NoStage(
                "no Odoo project on the task".to_string(),
            ));
        };

        // The pipeline's own `stage` wins over the configured default, which in
        // turn wins over the built-in name list.
        let preferred: Vec<String> = step
            .stage
            .clone()
            .into_iter()
            .chain(request.preferred.iter().cloned())
            .collect();

        let resolved = self
            .client
            .resolve_stage_for_project(project_id, request.kind, &preferred)
            .map_err(|err| err.to_string())?;

        let Some(target) = resolved else {
            return Ok(StageMove::NoStage(match step.stage {
                Some(name) => format!("no stage named \"{name}\" in {}", request.project_name),
                None => format!(
                    "no {} stage in {}",
                    match request.kind {
                        StageKind::Done => "QA-like",
                        StageKind::InProgress => "working",
                    },
                    request.project_name
                ),
            }));
        };
        if Some(target.id) == request.stage_id {
            return Ok(StageMove::Unchanged(target.name));
        }
        self.client
            .move_stage(request.task_id, target.id)
            .map_err(|err| err.to_string())?;
        Ok(StageMove::Moved(target.name))
    }

    fn post_comment(&self, task_id: i64, html: &str) -> Result<(), String> {
        self.client
            .post_comment(task_id, html)
            .map_err(|err| err.to_string())
    }

    fn set_task_state(&self, task_id: i64, state: &str) -> Result<(), String> {
        self.client
            .set_task_state(task_id, state)
            .map_err(|err| err.to_string())
    }
}
