//! The board tab's key map.
//!
//! Ported from the board arms of `handlePanelKey` / `onSelect` / `onExpand` in
//! the Node app's `src/tui/App.js`. Pure: every key either moves the cursor,
//! opens a dialog, or enqueues an [`Action`]. Nothing here blocks, so a whole
//! sequence can be replayed in a test and the resulting queue inspected.

use crossterm::event::{KeyCode, KeyEvent};

use crate::daemon::BoardFilter;
use crate::types::{NotificationStatus, Task};
use crate::ui::dialogs::{Dialog, NotifMenu, PipelineView, ProjectFilter, TaskMenu};
use crate::ui::state::{Action, AppState};

use super::start::{start, StartRequest};
use super::view::{snapshot, BoardRow, BoardSnapshot};
use super::{controller, detail, launch::LaunchKind, open};

/// Route one key on the board tab.
pub fn handle_board(state: &mut AppState, key: KeyEvent) {
    let snapshot = snapshot(state);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.board_sel.move_by(&snapshot.keys, -1),
        KeyCode::Down | KeyCode::Char('j') => state.board_sel.move_by(&snapshot.keys, 1),
        KeyCode::Enter => on_select(state, &snapshot),
        KeyCode::Right => on_expand(state, &snapshot, true),
        KeyCode::Left => on_expand(state, &snapshot, false),
        KeyCode::Char(ch) => on_char(state, ch, &snapshot),
        _ => {}
    }
}

fn on_char(state: &mut AppState, ch: char, snapshot: &BoardSnapshot) {
    let task = snapshot.row.task().cloned();
    match ch {
        's' => with_task(state, task, |state, task| {
            start(state, StartRequest::new(&task, LaunchKind::Task))
        }),
        'v' => with_task(state, task, |state, task| {
            start(
                state,
                StartRequest::new(
                    &task,
                    LaunchKind::Revision {
                        // The archive names the real one; this is a placeholder
                        // until the worker has looked it up.
                        session_id: String::new(),
                    },
                ),
            )
        }),
        // C reopens the conversation with no prompt — neither starting work nor
        // handing over revision notes, just picking the thread back up.
        'C' => with_task(state, task, |state, task| {
            start(
                state,
                StartRequest::new(
                    &task,
                    LaunchKind::Conversation {
                        session_id: String::new(),
                    },
                ),
            )
        }),
        'o' => with_task(state, task, |state, task| {
            let url = super::start::task_url(state, task.id);
            if url.is_empty() {
                state.flash("No Odoo URL configured — set odoo.url in ~/.claude-sessions.json.");
            } else {
                state.enqueue(Action::OpenUrl(url));
            }
        }),
        'm' => with_task(state, task, move_stage),
        'g' => with_task(state, task, go_to_session),
        'G' => with_task(state, task, focus_terminal),
        'P' => pipeline_view(state, snapshot),
        'S' => ssh(state, snapshot),
        'f' => {
            let next = match state.board.filter {
                BoardFilter::Mine => BoardFilter::All,
                BoardFilter::All => BoardFilter::Mine,
            };
            state.board.set_filter(next);
            refresh(state);
        }
        'p' => open(
            state,
            Dialog::ProjectFilter(ProjectFilter::new(&state.config)),
        ),
        'x' => dismiss(state, snapshot),
        // Both land with later phases; a key that silently does nothing reads
        // as broken, so they say when they arrive.
        'M' => state.flash("Your open merge requests land with the deploy phase."),
        'D' => state.flash("Daemon run logs land with the extras phase."),
        _ => {}
    }
}

fn with_task(state: &mut AppState, task: Option<Task>, run: impl FnOnce(&mut AppState, Task)) {
    if let Some(task) = task {
        run(state, task);
    }
}

pub(crate) fn refresh(state: &mut AppState) {
    let options = super::fetch_options(state);
    state.enqueue(Action::RefreshBoard(Box::new(options)));
    state.dirty = true;
}

fn on_select(state: &mut AppState, snapshot: &BoardSnapshot) {
    match &snapshot.row {
        BoardRow::Notification { id } => {
            let id = id.clone();
            read_notification(state, &id);
            if let Some(notif) = state.notification(&id).cloned() {
                let live = controller::resolve_notif_session(
                    state.sessions(),
                    &notif,
                    notif.task_id.and_then(|task| state.board.link(task)),
                )
                .map(|session| {
                    (
                        session.session_id.clone(),
                        session.session_file.clone(),
                        session.cwd.clone(),
                    )
                });
                open(state, Dialog::NotifMenu(NotifMenu::new(notif, live)));
            }
        }
        BoardRow::Project { .. } | BoardRow::Stage { .. } => {
            if let Some(key) = snapshot.row.expand_key() {
                state.board.toggle(&key);
                state.dirty = true;
            }
        }
        BoardRow::Task { task, .. } | BoardRow::Subtask { task, .. } => {
            let menu = TaskMenu::build(task, state);
            open(state, Dialog::TaskMenu(menu));
        }
        BoardRow::Inert => {}
    }
}

/// `→` expands, or previews when there is nothing left to expand; `←`
/// collapses, and on a subtask collapses the parent that holds it.
fn on_expand(state: &mut AppState, snapshot: &BoardSnapshot, expand: bool) {
    match &snapshot.row {
        BoardRow::Project { .. } | BoardRow::Stage { .. } => {
            if let Some(key) = snapshot.row.expand_key() {
                state.board.set_expanded(&key, expand);
                state.dirty = true;
            }
        }
        BoardRow::Task { task, has_subtasks } => {
            let key = snapshot.row.expand_key();
            match (expand, has_subtasks, key) {
                // Has subtasks and they are closed: open them first. A second
                // press shows the detail, so one key does both without a mode.
                (true, true, Some(key)) if !state.board.expanded.contains(&key) => {
                    state.board.expanded.insert(key);
                    state.dirty = true;
                }
                (true, _, _) => show_task(state, task),
                (false, true, Some(key)) => {
                    state.board.expanded.remove(&key);
                    state.dirty = true;
                }
                (false, _, _) => {}
            }
        }
        BoardRow::Subtask { task, parent_id } => {
            if expand {
                show_task(state, task);
            } else {
                state
                    .board
                    .expanded
                    .remove(&crate::board::subtask_key(*parent_id));
                state.dirty = true;
            }
        }
        BoardRow::Notification { id } => {
            if expand {
                let id = id.clone();
                read_notification(state, &id);
                show_notification(state, &id);
            }
        }
        BoardRow::Inert => {}
    }
}

fn show_task(state: &mut AppState, task: &Task) {
    state.board.detail = Some(detail::loading(task, detail::state_of(state, task.id)));
    state.flash = None;
    state.enqueue(Action::FetchTaskDescription { task_id: task.id });
    state.dirty = true;
}

fn show_notification(state: &mut AppState, id: &str) {
    let Some(notif) = state.notification(id).cloned() else {
        return;
    };
    let link = notif.task_id.and_then(|task| state.board.link(task));
    let has_session = controller::resolve_notif_session(state.sessions(), &notif, link).is_some();
    state.board.detail = Some(detail::notification(&notif, has_session));
    state.flash = None;
    state.dirty = true;
}

/// Looking at a notification marks it read. The engine owns the list, so the
/// change is also announced — mutating only the local copy is why a resolved
/// notification used to come back on the next reconnect.
fn read_notification(state: &mut AppState, id: &str) {
    if state.notification(id).map(|n| n.status) != Some(NotificationStatus::Unread) {
        return;
    }
    state.set_notification_status(id, NotificationStatus::Read);
    state.enqueue(Action::Notifications {
        ids: vec![id.to_string()],
        status: Some(NotificationStatus::Read),
    });
}

fn dismiss(state: &mut AppState, snapshot: &BoardSnapshot) {
    if let BoardRow::Notification { id } = &snapshot.row {
        let id = id.clone();
        state.dismiss_notification(&id);
        state.enqueue(Action::Notifications {
            ids: vec![id],
            status: None,
        });
    }
}

/// The two "go to the running session" entries, shared by the key map and the
/// task menu.
pub(crate) fn task_shortcut(
    state: &mut AppState,
    task: &Task,
    action: crate::ui::dialogs::TaskAction,
) {
    match action {
        crate::ui::dialogs::TaskAction::GoToSession => go_to_session(state, task.clone()),
        crate::ui::dialogs::TaskAction::FocusTerminal => focus_terminal(state, task.clone()),
        _ => {}
    }
}

pub(crate) fn move_stage(state: &mut AppState, task: Task) {
    state.enqueue(Action::FetchStages {
        task_id: task.id,
        project_id: task.project_id,
    });
    open(
        state,
        Dialog::StagePicker(crate::ui::dialogs::StagePicker::new(&task)),
    );
}

fn go_to_session(state: &mut AppState, task: Task) {
    let link = state.board.link(task.id);
    match controller::go_to_task_session(state.sessions(), task.id, link) {
        controller::SessionTarget::Session {
            session_id,
            session_file,
            project,
        } => crate::ui::keys::go_to_session(state, &session_id, session_file, &project),
        _ => state.flash(format!(
            "No live session for #{} (it may have ended).",
            task.id
        )),
    }
}

fn focus_terminal(state: &mut AppState, task: Task) {
    let link = state.board.link(task.id);
    match controller::focus_task_terminal(state.sessions(), task.id, link) {
        Ok(target) => {
            if target.others > 1 {
                state.flash(format!(
                    "{} sessions on this task — opening the most recent.",
                    target.others
                ));
            }
            state.enqueue(Action::FocusTerminal(Box::new(target.session)));
        }
        // A session with no controlling terminal is still running: name it
        // rather than implying there is none.
        Err(controller::SessionTarget::NoTerminal { session_id }) => state.flash(format!(
            "Session {} has no terminal to focus (started detached?).",
            super::spec::short(&session_id)
        )),
        Err(_) => state.flash(format!(
            "No live session for #{} (it may have ended).",
            task.id
        )),
    }
}

/// `P` — the pipeline that will actually run for this project, from the
/// project's own repo. A project with no repo mapped has nowhere to put a
/// `pipeline.json`, and the viewer says so rather than looking broken.
fn pipeline_view(state: &mut AppState, snapshot: &BoardSnapshot) {
    let Some(project) = snapshot.row.project_name().map(str::to_string) else {
        return;
    };
    let repo = state
        .config
        .odoo_project_dir_list(&project)
        .first()
        .cloned();
    open(state, Dialog::Pipeline(PipelineView::open(project, repo)));
}

fn ssh(state: &mut AppState, snapshot: &BoardSnapshot) {
    if let Some(project) = snapshot.row.project_name().map(str::to_string) {
        state.enqueue(Action::Ssh { project });
    }
}
