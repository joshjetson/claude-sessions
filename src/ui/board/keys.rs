//! The board tab's key map.
//!
//! Ported from the board arms of `handlePanelKey` / `onSelect` / `onExpand` in
//! the Node app's `src/tui/App.js`. Pure: every key either moves the cursor,
//! opens a dialog, or enqueues an [`Action`]. Nothing here blocks, so a whole
//! sequence can be replayed in a test and the resulting queue inspected.

use crossterm::event::{KeyCode, KeyEvent};

use crate::daemon::BoardFilter;
use crate::types::{NotificationStatus, Task};
use crate::ui::dialogs::{Dialog, NotifMenu, PipelineView, ProjectFilter, RunMenu, TaskMenu};
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
        'g' => with_task(state, task, |state, task| go_to_session(state, task.id)),
        'G' => with_task(state, task, |state, task| focus_terminal(state, task.id)),
        // R watches the selected stage as a QA run, or drops the run on a run
        // row. Read-only either way: it groups rows and reports, and starts
        // nothing.
        'R' => {
            let (project, stage, existing) = match &snapshot.row {
                BoardRow::QaRun { run_id, .. } => {
                    (String::new(), String::new(), Some(run_id.clone()))
                }
                BoardRow::Stage { project, stage } => (project.clone(), stage.clone(), None),
                BoardRow::Task { task, .. } | BoardRow::Subtask { task, .. } => {
                    (task.project_name.clone(), task.stage_name.clone(), None)
                }
                BoardRow::QaRunTask { run_id, .. } => {
                    (String::new(), String::new(), Some(run_id.clone()))
                }
                _ => {
                    state.flash(
                        "Select a stage, or a task in one, to watch it as a QA run.".to_string(),
                    );
                    state.dirty = true;
                    return;
                }
            };
            crate::ui::board::watch_or_drop(state, &project, &stage, existing);
        }
        // ] walks to the next agent waiting on a decision, across every run.
        ']' => crate::ui::board::jump_to_next_ask(state, &snapshot.keys),
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
        // Not a picker: Enter opens one in a browser and the dialog stays
        // open, so a morning's worth of review links can be opened in a row.
        'M' => {
            state.enqueue(Action::FetchOpenMrs);
            open(
                state,
                Dialog::OpenMrs(crate::ui::dialogs::OpenMrs::loading()),
            );
        }
        'D' => with_task(state, task, daemon_logs),
        _ => {}
    }
}

/// The auto-dev-daemon's run logs for a task. Read-only: this dashboard
/// observes that daemon, it does not drive it.
fn daemon_logs(state: &mut AppState, task: Task) {
    let dialog =
        crate::ui::dialogs::DaemonLogs::open(&state.paths.auto_dev_runs_dir, task.id, &task.tags);
    open(state, Dialog::DaemonLogs(dialog));
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
            let task_id = task.id;
            open(state, Dialog::TaskMenu(menu));
            // The menu opens with whatever the QAden cache already knew; the
            // `git rev-parse` that decides staleness runs on the worker and
            // swaps the label in when it lands (brief §10 mandate #9).
            state.enqueue(Action::RefreshQaState { task_id });
        }
        // A run row is a task row. Same menu, same keys — the run is a
        // grouping, not a different kind of thing.
        BoardRow::QaRunTask { task: Some(task), .. } => {
            let menu = TaskMenu::build(task, state);
            let task_id = task.id;
            open(state, Dialog::TaskMenu(menu));
            state.enqueue(Action::RefreshQaState { task_id });
        }
        // The board no longer carries this task: it left the stage and the run
        // kept it. There is nothing to build a menu from.
        BoardRow::QaRunTask { task: None, task_id, .. } => {
            let task_id = *task_id;
            state.flash = Some(format!(
                "Task {task_id} has left this stage — the run still covers it, but the board has no record to open."
            ));
            state.dirty = true;
        }
        BoardRow::QaRun { run_id, .. } => {
            let run_id = run_id.clone();
            open(state, Dialog::RunMenu(RunMenu::build(&run_id, state)));
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
        // Stored inverted, so a brand-new run renders open: the point of
        // creating one is to look at it.
        BoardRow::QaRun { run_id, .. } => {
            let key = crate::board::collapsed_key(run_id);
            state.board.set_expanded(&key, !expand);
            state.dirty = true;
        }
        BoardRow::QaRunTask { task, run_id, .. } => {
            if expand {
                if let Some(task) = task {
                    show_task(state, task);
                }
            } else {
                let key = crate::board::collapsed_key(run_id);
                state.board.set_expanded(&key, true);
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
    // Whatever the last task's pane was told is not about this one.
    state.board.detail_answers = crate::ui::board::DetailAnswers {
        task_id: Some(task.id),
        ..Default::default()
    };
    state.board.detail = Some(detail::loading(task, detail::state_of(state, task.id)));
    // A new pane starts at its top, unstuck — Node's `setDetail` did the same
    // (`controller.js:24-25`), and the scroll offset is shared with whatever the
    // pane showed before.
    state.conv.scroll_top = 0;
    state.conv.stick = false;
    state.flash = None;
    state.enqueue(Action::FetchTaskDescription { task_id: task.id });
    // Only when this install has Optics at all: the lookup is a round trip, and
    // the answer for an unconfigured install is always "nothing".
    if state.config.optics_api().is_some() && state.config.optics_token().is_some() {
        state.enqueue(Action::FetchTaskOptics {
            task_id: task.id,
            project: task.project_name.clone(),
        });
    }
    state.dirty = true;
}

fn show_notification(state: &mut AppState, id: &str) {
    let Some(notif) = state.notification(id).cloned() else {
        return;
    };
    let link = notif.task_id.and_then(|task| state.board.link(task));
    let has_session = controller::resolve_notif_session(state.sessions(), &notif, link).is_some();
    state.board.detail = Some(detail::notification(&notif, has_session));
    state.conv.scroll_top = 0;
    state.conv.stick = false;
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
        crate::ui::dialogs::TaskAction::GoToSession => go_to_session(state, task.id),
        crate::ui::dialogs::TaskAction::FocusTerminal => focus_terminal(state, task.id),
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

/// `g` — jump to the live session working a task. Shared with the Deploy tab,
/// which asks the same question of the same links, so the wording of "there
/// isn't one" is written once.
pub(crate) fn go_to_session(state: &mut AppState, task_id: i64) {
    let link = state.board.link(task_id);
    match controller::go_to_task_session(state.sessions(), task_id, link) {
        controller::SessionTarget::Session {
            session_id,
            session_file,
            project,
        } => crate::ui::keys::go_to_session(state, &session_id, session_file, &project),
        _ => state.flash(format!(
            "No live session for #{task_id} (it may have ended)."
        )),
    }
}

/// `G` — raise that session's terminal instead of its conversation.
pub(crate) fn focus_terminal(state: &mut AppState, task_id: i64) {
    let link = state.board.link(task_id);
    match controller::focus_task_terminal(state.sessions(), task_id, link) {
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
            "No live session for #{task_id} (it may have ended)."
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
