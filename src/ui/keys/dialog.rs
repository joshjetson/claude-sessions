//! What a dialog asked for, carried out.
//!
//! The dispatcher owns every state change a dialog causes, so a dialog cannot
//! replace itself halfway through a key and then be overwritten by the caller
//! putting the old one back. It lives beside the key router rather than inside
//! it because the board's menus turn into a fair amount of follow-up work.

use std::path::PathBuf;

use crate::ui::board::SessionTarget;
use crate::ui::dialogs::{
    ContextDialog, Dialog, DialogOutcome, FolderManager, TargetBranch, TaskAction, TaskCommand,
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
    }
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
        // Handled inside the menu itself.
        TaskAction::Start(_) | TaskAction::Later(_) | TaskAction::Cancel => {}
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
