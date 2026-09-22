//! The one way a task gets picked up.
//!
//! Every route into a launch — `s`, `v`, `C`, the task menu, the context
//! prompt, the two confirmation dialogs' overrides and the folder pickers —
//! ends here, so the guards cannot be bypassed by adding a new entry point. In
//! the Node app `spawnTaskPipeline`, `spawnQaPipeline`, `spawnRevision` and
//! `spawnRevisionInNewTab` each re-implemented most of this, which is how the
//! duplicate-start guard came to exist on three of the four.

use crate::term::SessionRef;
use crate::types::Task;
use crate::ui::dialogs::{AlreadyRunning, BlockedBy, Dialog, DirPicker, SavedDirPicker};
use crate::ui::state::{Action, AppState};

use super::controller::task_sessions;
use super::launch::{
    all_discovered_dirs, gate_start, prompt_context, resolve_task_dir, working_stage_move,
    DirChoice, Gate, LaunchKind,
};
use super::spec::{short, LaunchSpec, ResumePurpose, ResumeRequest};

/// What a dialog asks the board to do once the user has decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub task: Box<Task>,
    pub kind: LaunchKind,
    pub extra_context: String,
    /// Set by the two confirmation dialogs: the user saw the reason and said go
    /// anyway.
    pub force: bool,
    /// A folder already chosen, skipping resolution — how the pickers hand
    /// their answer back.
    pub dir: Option<String>,
    /// Extra prompt variables. Used by the QA-run coordinator, which is about a
    /// run rather than about the one task whose folder it borrows.
    pub extras: std::collections::BTreeMap<String, String>,
}

impl StartRequest {
    pub fn new(task: &Task, kind: LaunchKind) -> Self {
        StartRequest {
            task: Box::new(task.clone()),
            kind,
            extra_context: String::new(),
            force: false,
            dir: None,
            extras: std::collections::BTreeMap::new(),
        }
    }

    pub fn context(mut self, extra: impl Into<String>) -> Self {
        self.extra_context = extra.into();
        self
    }

    pub fn forced(mut self) -> Self {
        self.force = true;
        self
    }

    pub fn in_dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = Some(dir.into());
        self
    }
}

/// Run a start request: guards, then either a resume or a fresh launch.
pub fn start(state: &mut AppState, request: StartRequest) {
    if let Some(gate) = gate_start(
        &request.task,
        &request.kind,
        state.sessions(),
        request.force,
    ) {
        return open_gate(state, gate, request);
    }
    if request.kind.resumes() {
        return resume(state, request);
    }
    if let Some(dir) = resolve_dir(state, &request) {
        launch_in(state, &request, &dir);
    }
}

/// The folder, or `None` when a picker was opened to ask for it.
fn resolve_dir(state: &mut AppState, request: &StartRequest) -> Option<String> {
    if let Some(dir) = request.dir.clone() {
        return Some(dir);
    }
    let discovered = all_discovered_dirs(&state.discovered_dirs);
    match resolve_task_dir(&state.config, &request.task.project_name, &discovered) {
        DirChoice::Known(dir) => Some(dir),
        DirChoice::ChooseSaved(saved) => {
            super::open(
                state,
                Dialog::SavedDirPicker(SavedDirPicker::new(saved, request.clone())),
            );
            None
        }
        DirChoice::ChooseAny { guess } => {
            super::open(
                state,
                Dialog::DirPicker(DirPicker::for_launch(discovered, guess, request.clone())),
            );
            None
        }
    }
}

fn open_gate(state: &mut AppState, gate: Gate, request: StartRequest) {
    match gate {
        Gate::BlockedBy { blocker_ids, open } => {
            state.enqueue(Action::FetchBlockers {
                task_id: request.task.id,
                blocker_ids,
            });
            super::open(state, Dialog::BlockedBy(BlockedBy::new(request, open)));
        }
        Gate::AlreadyRunning { running } => super::open(
            state,
            Dialog::AlreadyRunning(AlreadyRunning::new(request, running)),
        ),
    }
}

/// `v` and `C`: pick an existing conversation back up.
///
/// A live session is typed into rather than resumed — resuming its id in a new
/// tab starts a second process against the same conversation. Finding the
/// ARCHIVED one is file I/O with a self-healing copy in it, so that half goes
/// to the worker.
fn resume(state: &mut AppState, request: StartRequest) {
    let task = request.task.clone();
    let revision = matches!(request.kind, LaunchKind::Revision { .. });
    let live = task_sessions(state.sessions(), task.id);

    if let Some(session) = live.first() {
        if !revision {
            // Its terminal is already open — go there rather than starting a
            // second process against the same session id.
            let reference = SessionRef::from_session(session);
            state.flash(format!(
                "#{} is still running — opening its terminal.",
                task.id
            ));
            state.enqueue(Action::FocusTerminal(Box::new(reference)));
            return;
        }
        if let Some(spec) = send_to_live(state, &task, &request, session) {
            if live.len() > 1 {
                state.flash(format!(
                    "{} sessions on #{} — sending to the most recent.",
                    live.len(),
                    task.id
                ));
            }
            state.enqueue(Action::SendToSession(Box::new(spec)));
            return;
        }
        // A session with no controlling terminal cannot be typed into whatever
        // the environment, so name it and fall through to the new-tab path.
        state.flash(format!(
            "Session {} has no terminal to type into — opening a new tab instead.",
            short(&session.session_id)
        ));
    }

    let prompt = prompt_context(
        &state.config,
        &state.paths,
        &task,
        task_url(state, task.id),
        &request.extra_context,
        &request.extras,
    );
    state.enqueue(Action::Resume(Box::new(ResumeRequest {
        prompt,
        purpose: if revision {
            ResumePurpose::Revision
        } else {
            ResumePurpose::Conversation
        },
        link_cwd: state
            .board
            .link(task.id)
            .map(|link| link.cwd.clone())
            .unwrap_or_default(),
        stage_move: working_stage_move(&state.config, &task, &request.kind, None),
        known_session_ids: state.sessions().map(|s| s.session_id.clone()).collect(),
    })));
}

/// The revision typed into a session that is already open, when it has a
/// terminal to type into.
fn send_to_live(
    state: &AppState,
    task: &Task,
    request: &StartRequest,
    session: &crate::types::Session,
) -> Option<super::spec::SendSpec> {
    let reference = SessionRef::from_session(session);
    reference.tty_device()?;
    let cwd = session.cwd.clone();
    let kind = LaunchKind::Revision {
        session_id: session.session_id.clone(),
    };
    let prompt = prompt_context(
        &state.config,
        &state.paths,
        task,
        task_url(state, task.id),
        &request.extra_context,
        &request.extras,
    )
    .prompt(&kind, &cwd, None)
    .ok()??;
    Some(super::spec::SendSpec {
        task_id: task.id,
        session: reference,
        session_id: session.session_id.clone(),
        prompt,
        stage_move: working_stage_move(&state.config, task, &kind, Some(&cwd)),
    })
}

/// Everything after the folder is known.
fn launch_in(state: &mut AppState, request: &StartRequest, dir: &str) {
    let task = &request.task;
    let context = prompt_context(
        &state.config,
        &state.paths,
        task,
        task_url(state, task.id),
        &request.extra_context,
        &request.extras,
    );
    let prompt = match context.prompt(&request.kind, dir, None) {
        Ok(prompt) => prompt,
        Err(error) => return state.flash(error.to_string()),
    };
    let spec = LaunchSpec {
        task_id: task.id,
        cwd: dir.to_string(),
        flags: request.kind.flags(),
        prompt,
        title: format!("task-{}", task.id),
        stage_move: working_stage_move(&state.config, task, &request.kind, Some(dir)),
        known_session_ids: state.sessions().map(|s| s.session_id.clone()).collect(),
        say: format!("Started #{} in {dir}.", task.id),
        // A coordinator launch already carries the run id as a prompt variable.
        // Reusing it here avoids a second way of saying the same thing that
        // could drift from the first.
        run_id: request
            .extras
            .get(crate::pipeline::definitions::RUN_ID_VAR)
            .map(|id| crate::qarun::encode_run_id(id)),
        // A QA session gets one; a coordinator never does. A coordinator
        // holding a token could sign in the reviewer's name, which is the one
        // thing this is meant to prevent.
        reviewer_token: match request.kind {
            crate::ui::board::LaunchKind::Qa => Some(reviewer_token(task.id)),
            _ => None,
        },
    };
    // A fresh attempt clears any prior needs-info flag; the gate raises it
    // again if it still applies.
    state.board.blocked_tasks.remove(&task.id);
    state.enqueue(Action::Launch(Box::new(spec)));
    state.dirty = true;
}

/// A token for one QA session, proving later that an instruction came from the
/// reviewer rather than from another agent.
///
/// Not a secret in the cryptographic sense — any process running as this user
/// can read another's environment. It exists to stop CONFABULATION: a
/// coordinator once opened a relay with "the reviewer wants this finished
/// without waiting on them", an inference stated as the reviewer's words, and
/// the receiving agent refused it because it could not tell the two apart. A
/// token it has no reason to go and copy makes them tellable apart.
fn reviewer_token(task_id: i64) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    task_id.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("rv-{:016x}", hasher.finish())
}

/// The Odoo task URL.
///
/// Load-bearing beyond being a link: the launch prompt carries it, and the
/// scanner recovers a session's task by finding it in the transcript head.
pub fn task_url(state: &AppState, task_id: i64) -> String {
    let url = state.config.odoo_creds().url;
    if url.is_empty() {
        return String::new();
    }
    format!("{url}/web#id={task_id}&model=project.task&view_type=form")
}
