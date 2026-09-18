//! Deciding what picking up a task means, without doing any of it.
//!
//! Ported from `spawnTaskPipeline` / `spawnQaPipeline` / `spawnRevision` /
//! `resumeTaskConversation` in the Node app's `src/tui/actions.js`. Everything
//! here is a pure function: the guards return what to ask, and [`spec`] turns
//! the answer into a [`super::LaunchSpec`] the worker executes. Nothing in this
//! file spawns, writes or talks to Odoo, which is why the two guards below can
//! be tested with no machine that could run an agent against real task data.
//!
//! Both guards are incident reports:
//!
//! * The dependency gate. Without it the dashboard dispatches an agent against
//!   a data model its blocker has not built yet, producing a plausible-looking
//!   merge request on the wrong foundation — the most expensive kind of run.
//! * The duplicate-start guard. Nine agents once raced on one task in a single
//!   working tree, each creating the same branch and overwriting the others'
//!   work. It needs a task->session link that survives a daemon restart, which
//!   is why it could not exist until sessions carried their own task id.

use std::collections::BTreeMap;

use crate::config::ConfigHandle;
use crate::daemon::{StageMoveRequest, MOVE_IN_PROGRESS_STEP};
use crate::pipeline::BranchFallback;
use crate::types::{Session, SessionStatus, Task};
use crate::util::{join_dir, normalise_name, path_leaf};

use super::controller::task_sessions;
use super::spec::PromptContext;

/// Which pipeline a launch runs, and therefore how it differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchKind {
    /// `s` — the full task pipeline.
    Task,
    /// `v` — the revision instructions, typed into a live session or resumed
    /// into a new tab.
    Revision { session_id: String },
    /// The QA pass. No `--dangerously-skip-permissions`: QA drives real preview
    /// environments carrying live credentials, so a send-shaped button stops
    /// and asks rather than clicking.
    Qa,
    /// The developer-facing QA variant: reports in the terminal, writes no
    /// hand-back note.
    QaDry,
    /// The coordinating session for a QA run. It watches several passes; it runs
    /// none of them itself, and it spawns nothing — admission lives outside any
    /// model.
    QaRun,
    /// The other end of a task's life — a brief before anything is built.
    PreOptics,
    /// `C` — reopen the conversation and send NOTHING. Not a pipeline.
    Conversation { session_id: String },
    /// `R` on the Deploy tab — reconcile a merge request's conflicts. Prefers
    /// the session that wrote the branch, because it already knows why every
    /// hunk looks the way it does; falls back to a fresh one.
    Conflict { session_id: String },
}

impl LaunchKind {
    pub fn pipeline_id(&self) -> Option<&'static str> {
        match self {
            LaunchKind::Task => Some("task"),
            LaunchKind::Revision { .. } => Some("revision"),
            LaunchKind::Qa => Some("qa"),
            LaunchKind::QaDry => Some("qa-dry"),
            LaunchKind::QaRun => Some("qa-run"),
            LaunchKind::PreOptics => Some("pre-optics"),
            LaunchKind::Conflict { .. } => Some("conflict"),
            LaunchKind::Conversation { .. } => None,
        }
    }

    /// The flags after `claude`.
    pub fn flags(&self) -> String {
        const SKIP: &str = "--dangerously-skip-permissions";
        match self {
            LaunchKind::Task => SKIP.to_string(),
            LaunchKind::Revision { session_id } | LaunchKind::Conversation { session_id } => {
                format!("--resume {session_id} {SKIP}")
            }
            // The one resume that may have nothing to resume: a conflicted MR
            // whose task was never archived still needs resolving.
            LaunchKind::Conflict { session_id } if session_id.is_empty() => SKIP.to_string(),
            LaunchKind::Conflict { session_id } => format!("--resume {session_id} {SKIP}"),
            // See the note on the variant: QA answers prompts rather than
            // skipping them.
            // See the note on the Qa variant: these answer prompts rather than
            // skipping them. The coordinator holds no permission of its own —
            // it never opens the application and never edits anything.
            LaunchKind::Qa | LaunchKind::QaDry | LaunchKind::PreOptics | LaunchKind::QaRun => {
                String::new()
            }
        }
    }

    /// The two built-in pipelines word this differently; the difference is
    /// preserved because the prompts are golden-mastered.
    pub fn branch_fallback(&self) -> BranchFallback {
        match self {
            LaunchKind::Task => BranchFallback::Development,
            _ => BranchFallback::Default,
        }
    }

    /// Whether picking this up means work is starting.
    ///
    /// QA does not move the stage: the QA stage IS the working stage, a live
    /// session already shows on the row, and moving would encode the same fact
    /// twice and strand it if the session died. Resuming a conversation does
    /// not either — reading a conversation is not starting work on it.
    pub fn moves_to_working_stage(&self) -> bool {
        matches!(self, LaunchKind::Task | LaunchKind::Revision { .. })
    }

    /// Whether a session already on the task should stop this.
    ///
    /// A dry run and a pre-work brief skip the guard on purpose: a developer
    /// testing their own work legitimately has their own session open, which is
    /// the exact condition the guard exists to stop.
    pub fn guards_duplicates(&self) -> bool {
        matches!(self, LaunchKind::Task | LaunchKind::Qa)
    }

    /// Only the task pipeline refuses to start against unlanded work. A blocked
    /// task can still be reviewed, and often should be.
    pub fn honours_blockers(&self) -> bool {
        matches!(self, LaunchKind::Task)
    }

    /// Whether this kind picks a conversation back up rather than starting one.
    pub fn resumes(&self) -> bool {
        matches!(
            self,
            LaunchKind::Revision { .. }
                | LaunchKind::Conversation { .. }
                | LaunchKind::Conflict { .. }
        )
    }
}

/// A session already working the task, as the confirmation dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RacingSession {
    pub session_id: String,
    pub cwd: String,
    pub status: SessionStatus,
    pub last_timestamp: Option<String>,
}

impl RacingSession {
    pub fn of(session: &Session) -> Self {
        RacingSession {
            session_id: session.session_id.clone(),
            cwd: session.cwd.clone(),
            status: session.status,
            last_timestamp: session.last_timestamp.clone(),
        }
    }
}

/// Something to confirm before anything is launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Odoo says work this depends on has not landed.
    BlockedBy { blocker_ids: Vec<i64>, open: u32 },
    /// Sessions are already on this task, newest first.
    AlreadyRunning { running: Vec<RacingSession> },
}

/// Both guards, in the Node order. `force` is the deliberate override taken
/// from either dialog.
pub fn gate_start<'a>(
    task: &Task,
    kind: &LaunchKind,
    sessions: impl Iterator<Item = &'a Session> + Clone,
    force: bool,
) -> Option<Gate> {
    if force {
        return None;
    }
    if kind.honours_blockers() && task.open_blocker_count > 0 {
        return Some(Gate::BlockedBy {
            blocker_ids: task.blocked_by.clone(),
            open: task.open_blocker_count,
        });
    }
    if kind.guards_duplicates() {
        let running = task_sessions(sessions, task.id);
        if !running.is_empty() {
            return Some(Gate::AlreadyRunning {
                running: running.into_iter().map(RacingSession::of).collect(),
            });
        }
    }
    None
}

/// Which local repo folder a task runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirChoice {
    /// Exactly one saved folder: used silently, no prompt.
    Known(String),
    /// Two or more saved: pick one, or add another.
    ChooseSaved(Vec<String>),
    /// None saved: pick from every discovered folder, and remember the choice.
    ChooseAny { guess: String },
}

pub fn resolve_task_dir(
    config: &ConfigHandle,
    project_name: &str,
    discovered: &[String],
) -> DirChoice {
    let saved = config.odoo_project_dir_list(project_name);
    match saved.len() {
        1 => DirChoice::Known(saved[0].clone()),
        0 => DirChoice::ChooseAny {
            guess: guess_dir_for_project(project_name, discovered),
        },
        _ => DirChoice::ChooseSaved(saved.to_vec()),
    }
}

/// The discovered folder whose basename looks like the project's name.
///
/// Uses the crate's one [`normalise_name`] — the Node app had four copies of
/// this rule (optics, ssh, actions, purge), which is why a project could match
/// in three of them and not the fourth.
pub fn guess_dir_for_project(project_name: &str, discovered: &[String]) -> String {
    let target = normalise_name(project_name);
    if target.is_empty() {
        return String::new();
    }
    discovered
        .iter()
        .find(|path| {
            let base = normalise_name(path_leaf(path));
            !base.is_empty() && (base == target || base.contains(&target) || target.contains(&base))
        })
        .cloned()
        .unwrap_or_default()
}

/// Every folder the scanner found inside the configured groups, flattened.
pub fn all_discovered_dirs(discovered: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    discovered
        .iter()
        .flat_map(|(group, dirs)| dirs.iter().map(move |dir| join_dir(group, dir)))
        .collect()
}

/// The run-specific prompt values for a task, read off config once.
/// The variables a prompt is rendered with.
///
/// `caller_extras` are the ones the START REQUEST carried, and they are merged
/// LAST so a caller can override a task-derived default. They were dropped
/// entirely once: the coordinator launch sets `runId`, `taskIds`, `qaRoot` and
/// `triage` here, this function built its extras from the task alone, and the
/// coordinator rendered its prompt with none of them.
///
/// The failure was quiet and expensive. With no `taskIds` the coordinator could
/// not tell which tasks were in its run, so it guessed from file timestamps and
/// reported eleven tasks for a run of seven. With no `triage` it took the
/// shadow branch of its own prompt and answered nothing, while the run's mode
/// said triage. Nothing errored; it simply did the wrong job carefully.
pub fn prompt_context(
    config: &ConfigHandle,
    paths: &crate::paths::Paths,
    task: &Task,
    task_url: String,
    extra_context: &str,
    caller_extras: &BTreeMap<String, String>,
) -> PromptContext {
    PromptContext {
        task_id: task.id,
        task_url,
        extra_context: extra_context.to_string(),
        summary_file: paths
            .task_summary_file(task.id)
            .to_string_lossy()
            .into_owned(),
        target_branch: config.target_branch(&task.project_name).map(str::to_string),
        extras: [
            ("taskName", task.name.clone()),
            ("projectName", task.project_name.clone()),
            ("stageName", task.stage_name.clone()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .chain(
            caller_extras
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .collect(),
        // Filled in only by the conflict flow, which knows its merge request.
        mr: None,
    }
}

/// The work-ward stage move that rides along with a launch, or nothing.
pub fn working_stage_move(
    config: &ConfigHandle,
    task: &Task,
    kind: &LaunchKind,
    repo: Option<&str>,
) -> Option<StageMoveRequest> {
    if !kind.moves_to_working_stage() {
        return None;
    }
    let pipeline_id = kind.pipeline_id()?;
    Some(StageMoveRequest {
        project_id: Some(task.project_id).filter(|id| *id != 0),
        stage_id: Some(task.stage_id).filter(|id| *id != 0),
        project_name: task.project_name.clone(),
        repo_path: repo.map(std::path::PathBuf::from),
        preferred: config
            .in_progress_stage()
            .map(<[String]>::to_vec)
            .unwrap_or_default(),
        step_id: MOVE_IN_PROGRESS_STEP.to_string(),
        ..StageMoveRequest::to_in_progress(task.id, pipeline_id)
    })
}
