//! What a dialog asked for, carried out.
//!
//! The dispatcher owns every state change a dialog causes, so a dialog cannot
//! replace itself halfway through a key and then be overwritten by the caller
//! putting the old one back. It lives beside the key router rather than inside
//! it because the board's menus turn into a fair amount of follow-up work.

use std::path::PathBuf;

use crate::ui::board::SessionTarget;
use crate::ui::dialogs::{
    ContextDialog, DeployConfig, DeployTaskAction, DeployTaskCommand, Dialog, DialogOutcome,
    FolderManager, MergeConfirm, TargetBranch, TaskAction, TaskCommand,
};
use crate::ui::state::{Action, AppState, Pane, View};

/// Everything a dialog can ask for, in one place.
///
/// The dispatcher owns every state change so a dialog cannot replace itself
/// halfway through a key and then be overwritten by the caller putting the old
/// one back.
pub(super) fn apply_dialog_outcome(state: &mut AppState, dialog: Dialog, outcome: DialogOutcome) {
    match outcome {
        DialogOutcome::Stay => state.dialog = Some(dialog),
        DialogOutcome::Close => {}
        DialogOutcome::Act(action) => state.enqueue(action),
        DialogOutcome::Quit(quit) => state.quit = Some(quit),
        DialogOutcome::Flash(message) => state.flash(message),
        DialogOutcome::Keep { action, flash } => {
            if let Some(message) = flash {
                state.flash(message);
            }
            if let Some(action) = action {
                state.enqueue(action);
            }
            state.dialog = Some(dialog);
        }
        DialogOutcome::Run(command) => crate::ui::board::run_command(state, *command),
        DialogOutcome::Start(request) => crate::ui::board::start(state, *request),
        DialogOutcome::PickDir(picked) => {
            // Remembered, so the next launch for this project needs no prompt.
            let _ = state
                .config
                .add_odoo_project_dir(&picked.project, &picked.dir);
            if let Some(request) = picked.request {
                crate::ui::board::start(state, request.in_dir(picked.dir));
            }
        }
        DialogOutcome::AddFolder(request) => {
            let discovered = crate::ui::board::all_discovered_dirs(&state.discovered_dirs);
            let guess =
                crate::ui::board::guess_dir_for_project(&request.task.project_name, &discovered);
            state.dialog = Some(Dialog::DirPicker(
                crate::ui::dialogs::DirPicker::for_launch(discovered, guess, *request),
            ));
            state.dirty = true;
        }
        DialogOutcome::Task(command) => run_task_command(state, *command),
        DialogOutcome::GoTo(target) => {
            if let SessionTarget::Session {
                session_id,
                session_file,
                project,
            } = *target
            {
                go_to_session(state, &session_id, session_file, &project);
            }
        }
        DialogOutcome::FilterChanged => {
            let filter = state.board.filter;
            state.board.set_filter(filter);
            crate::ui::board::keys::refresh(state);
        }
        DialogOutcome::Deploy(command) => {
            crate::ui::dialogs::deploy::run_deploy_command(state, &command.project, command.action)
        }
        DialogOutcome::DeployTask(command) => run_deploy_task_command(state, *command),
        DialogOutcome::ResolveConflicts(task) => resolve_conflicts(state, &task),
        DialogOutcome::Configure(project) => {
            state.dialog = Some(Dialog::DeployConfig(DeployConfig::new(&project, state)));
            state.dirty = true;
        }
    }
}

/// The Deploy task menu's entries, carried out.
fn run_deploy_task_command(state: &mut AppState, command: DeployTaskCommand) {
    let task = *command.task;
    match command.action {
        DeployTaskAction::Merge => {
            let readiness = crate::deploy::merge_readiness(&task);
            if readiness.ready {
                state.dialog = Some(Dialog::MergeConfirm(MergeConfirm::new(&task)));
                state.dirty = true;
            } else {
                // The blocker, not "cannot merge": the reason is the point.
                state.flash(format!("Cannot merge #{}: {}", task.id, readiness.reason));
            }
        }
        DeployTaskAction::ResolveConflicts => {
            state.dialog = Some(Dialog::ResolveConflict(
                crate::ui::dialogs::ResolveConflictConfirm::build(&task, state),
            ));
            state.dirty = true;
        }
        DeployTaskAction::GoToSession => crate::ui::board::keys::go_to_session(state, task.id),
        DeployTaskAction::OpenMr => state.enqueue(Action::OpenUrl(task.mr_url.clone())),
        DeployTaskAction::OpenTask => {
            let url = crate::ui::board::task_url(state, task.id);
            if url.is_empty() {
                state.flash("No Odoo URL configured — set odoo.url in ~/.claude-sessions.json.");
            } else {
                state.enqueue(Action::OpenUrl(url));
            }
        }
        // The stage picker is the board's, and it takes a board task — the
        // Deploy tab knows the id and the project, which is all it needs.
        DeployTaskAction::MoveStage => {
            let stub = crate::types::Task {
                id: task.id,
                name: task.name.clone(),
                project_id: task.project_id,
                project_name: task.project_name.clone(),
                stage_name: task.stage_name.clone(),
                ..crate::types::Task::default()
            };
            crate::ui::board::keys::move_stage(state, stub);
        }
        DeployTaskAction::DeployProject => {
            let project = task.project_name.clone();
            state.dialog = Some(Dialog::DeployConfirm(
                crate::ui::dialogs::DeployConfirm::build(&project, state),
            ));
            state.dirty = true;
        }
        DeployTaskAction::Close => {}
    }
}

/// Hand a conflicted merge request to an agent.
///
/// Goes through the same `Resume` action `v` and `C` use, so the archive
/// lookup — which self-heals a stale record and restores the transcript Claude
/// Code needs on disk for `--resume` — happens exactly once in this codebase.
fn resolve_conflicts(state: &mut AppState, task: &crate::types::DeployTask) {
    let Some(mr) = &task.mr else {
        return state.flash(format!("#{} has no merge request loaded.", task.id));
    };
    let stub = crate::types::Task {
        id: task.id,
        name: task.name.clone(),
        project_id: task.project_id,
        project_name: task.project_name.clone(),
        stage_name: task.stage_name.clone(),
        ..crate::types::Task::default()
    };
    let mut prompt = crate::ui::board::prompt_context(
        &state.config,
        &state.paths,
        &stub,
        crate::ui::board::task_url(state, task.id),
        "",
    );
    prompt.mr = Some(crate::pipeline::MergeRequestVars {
        iid: mr.iid,
        url: task.mr_url.clone(),
        source_branch: mr.source_branch.clone(),
        target_branch: mr.target_branch.clone(),
    });
    // Where the agent will run: the folder the dashboard last saw this task
    // worked in, else the project's first configured repository. The archive
    // record wins over both when it names one.
    let link_cwd = state
        .board
        .link(task.id)
        .map(|link| link.cwd.clone())
        .filter(|cwd| !cwd.is_empty())
        .or_else(|| {
            state
                .config
                .odoo_project_dir_list(&task.project_name)
                .first()
                .cloned()
        })
        .unwrap_or_default();
    if link_cwd.is_empty() {
        return state.flash(format!(
            "No folder known for {} — add one with the task menu's repo folders.",
            task.project_name
        ));
    }
    state.enqueue(Action::Resume(Box::new(crate::ui::board::ResumeRequest {
        prompt,
        purpose: crate::ui::board::ResumePurpose::Conflict,
        link_cwd,
        // Conflict resolution never moves the stage: the task is already in
        // Deployed and the merge is what is outstanding, not the work.
        stage_move: None,
        known_session_ids: state.sessions().map(|s| s.session_id.clone()).collect(),
    })));
}

/// The task-menu entries the board, not the dialog, carries out.
fn run_task_command(state: &mut AppState, command: TaskCommand) {
    let task = *command.task;
    match command.action {
        TaskAction::GoToSession | TaskAction::FocusTerminal => {
            crate::ui::board::keys::task_shortcut(state, &task, command.action)
        }
        TaskAction::Context { revision } => {
            state.dialog = Some(Dialog::Context(ContextDialog::new(&task, revision)));
            state.dirty = true;
        }
        TaskAction::MoveStage => crate::ui::board::keys::move_stage(state, task),
        TaskAction::OpenInBrowser => {
            let url = crate::ui::board::task_url(state, task.id);
            if url.is_empty() {
                state.flash("No Odoo URL configured — set odoo.url in ~/.claude-sessions.json.");
            } else {
                state.enqueue(Action::OpenUrl(url));
            }
        }
        TaskAction::RepoFolders => {
            let saved = state
                .config
                .odoo_project_dir_list(&task.project_name)
                .to_vec();
            let discovered = crate::ui::board::all_discovered_dirs(&state.discovered_dirs);
            state.dialog = Some(Dialog::FolderManager(FolderManager::new(
                task.project_name.clone(),
                saved,
                discovered,
            )));
            state.dirty = true;
        }
        TaskAction::TargetBranch => {
            let current = state
                .config
                .target_branch(&task.project_name)
                .map(str::to_string);
            state.dialog = Some(Dialog::TargetBranch(TargetBranch::new(
                task.project_name.clone(),
                current.as_deref(),
            )));
            state.dirty = true;
        }
        TaskAction::DaemonLogs => {
            state.dialog = Some(Dialog::DaemonLogs(crate::ui::dialogs::DaemonLogs::open(
                &state.paths.auto_dev_runs_dir,
                task.id,
                &task.tags,
            )));
            state.dirty = true;
        }
        // Handled inside the menu itself.
        TaskAction::Start(_) | TaskAction::Cancel => {}
    }
}

/// Switch to the sessions view and open a conversation. Shared by the board's
/// `g`, the task menu and the notification menu.
pub fn go_to_session(
    state: &mut AppState,
    session_id: &str,
    session_file: Option<PathBuf>,
    project: &str,
) {
    state.view = View::Sessions;
    state.focus = Pane::Tree;
    state.expanded_projects.insert(project.to_string());
    state.tree_sel.set(&[format!("s:{session_id}")], 0);
    state.flash = None;
    state.selected_session_id = Some(session_id.to_string());
    state.selected_session_file = session_file.clone();
    state.conv.stick = true;
    state.conv.scroll_top = 0;
    state.enqueue(Action::SelectSession {
        session_id: session_id.to_string(),
        session_file,
    });
    state.dirty = true;
}
