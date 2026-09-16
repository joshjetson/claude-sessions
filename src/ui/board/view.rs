//! The board tab's list: the notification feed above the Odoo task tree.
//!
//! Ported from `buildList`'s board arm in the Node app's `src/tui/App.js` and
//! `buildNotifItems` in `treefmt.js`. The rows themselves are built by
//! [`crate::board`], which is pure and already tested; this is the part that
//! knows where the rows come from and which one the cursor is on.

use std::collections::VecDeque;

use ratatui::text::Line;

use crate::board::{
    board_item_key, build_board_tree, format_board_item, project_key, stage_key, subtask_key,
    BoardItem,
};
use crate::types::{Notification, NotificationStatus, Task};
use crate::ui::spans::row_line;
use crate::ui::state::AppState;

use super::slice::{live_task_ids, BoardSlice};

/// The feed rows: a header, the unresolved notifications, and a blank line
/// separating them from the tree. Resolved notifications are gone, not dimmed —
/// resolving one is how you make it go away.
fn notification_items(notifications: &VecDeque<Notification>) -> Vec<BoardItem<'_>> {
    let active: Vec<&Notification> = notifications
        .iter()
        .filter(|n| n.status != NotificationStatus::Resolved)
        .collect();
    if active.is_empty() {
        return Vec::new();
    }
    let mut items = vec![BoardItem::NotificationHeader {
        unread: active
            .iter()
            .filter(|n| n.status == NotificationStatus::Unread)
            .count(),
        total: active.len(),
    }];
    items.extend(
        active
            .into_iter()
            .map(|notif| BoardItem::Notification { notif }),
    );
    items.push(BoardItem::Separator);
    items
}

/// Messages that stand in for the tree: still loading, or Odoo said no.
const LOADING: &str = "Loading tasks from Odoo…";
const RETRY: &str = "Press r to retry";
const NO_CREDENTIALS: &str = "No Odoo credentials — set the odoo block in ~/.claude-sessions.json.";
const NOTHING: &str = "Nothing assigned in the visible stages.";
const TRUNCATED: &str = "… list truncated; switch filter (f) or narrow the projects (p)";

/// Every row of the board tab, in order.
///
/// Borrows from the state it is built out of, so a list costs no clones: it is
/// rebuilt on every draw and never outlives the frame.
pub fn build_items<'a>(
    board: &'a BoardSlice,
    notifications: &'a VecDeque<Notification>,
) -> Vec<BoardItem<'a>> {
    let mut items = notification_items(notifications);
    match (&board.board, &board.error) {
        (_, Some(error)) => {
            items.push(BoardItem::Info { name: error });
            items.push(BoardItem::Info { name: RETRY });
        }
        (None, None) if board.loading => items.push(BoardItem::Info { name: LOADING }),
        (None, None) => items.push(BoardItem::Info {
            name: NO_CREDENTIALS,
        }),
        (Some(board_data), None) => {
            if board_data.projects.is_empty() {
                items.push(BoardItem::Info { name: NOTHING });
            } else {
                items.extend(build_board_tree(board_data, &board.expanded));
            }
            if board_data.truncated {
                items.push(BoardItem::Info { name: TRUNCATED });
            }
        }
    }
    items
}

/// The row the cursor is on, owned so the caller can mutate state afterwards.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardRow {
    Project {
        name: String,
    },
    Stage {
        project: String,
        stage: String,
    },
    Task {
        task: Box<Task>,
        has_subtasks: bool,
    },
    Subtask {
        task: Box<Task>,
        parent_id: i64,
    },
    Notification {
        id: String,
    },
    /// A header, separator or message — nothing to act on.
    Inert,
}

impl BoardRow {
    fn of(item: &BoardItem<'_>) -> BoardRow {
        match item {
            BoardItem::Project { name, .. } => BoardRow::Project {
                name: (*name).to_string(),
            },
            BoardItem::Stage {
                project_name,
                stage_name,
                ..
            } => BoardRow::Stage {
                project: (*project_name).to_string(),
                stage: (*stage_name).to_string(),
            },
            BoardItem::Task {
                task, sub_count, ..
            } => BoardRow::Task {
                task: Box::new((*task).clone()),
                has_subtasks: *sub_count > 0,
            },
            BoardItem::Subtask { task, parent_id } => BoardRow::Subtask {
                task: Box::new((*task).clone()),
                parent_id: *parent_id,
            },
            BoardItem::Notification { notif } => BoardRow::Notification {
                id: notif.id.clone(),
            },
            _ => BoardRow::Inert,
        }
    }

    /// The task this row is about — task and subtask rows both answer.
    pub fn task(&self) -> Option<&Task> {
        match self {
            BoardRow::Task { task, .. } | BoardRow::Subtask { task, .. } => Some(task),
            _ => None,
        }
    }

    /// The Odoo project this row belongs to. Project and stage rows answer for
    /// themselves, so `S` and `P` work from anywhere in a project's block
    /// without moving the cursor back to its header.
    pub fn project_name(&self) -> Option<&str> {
        match self {
            BoardRow::Project { name } => Some(name),
            BoardRow::Stage { project, .. } => Some(project),
            BoardRow::Task { task, .. } | BoardRow::Subtask { task, .. } => {
                Some(&task.project_name)
            }
            _ => None,
        }
    }

    /// The expansion key this row toggles, if it has one.
    pub fn expand_key(&self) -> Option<String> {
        match self {
            BoardRow::Project { name } => Some(project_key(name)),
            BoardRow::Stage { project, stage } => Some(stage_key(project, stage)),
            BoardRow::Task { task, has_subtasks } if *has_subtasks => Some(subtask_key(task.id)),
            BoardRow::Subtask { parent_id, .. } => Some(subtask_key(*parent_id)),
            _ => None,
        }
    }
}

/// What the key handler reads: the row keys, where the cursor is, and what is
/// under it.
pub struct BoardSnapshot {
    pub keys: Vec<String>,
    pub selected: usize,
    pub row: BoardRow,
}

pub fn snapshot(state: &AppState) -> BoardSnapshot {
    let items = build_items(&state.board, &state.notifications);
    let keys: Vec<String> = items.iter().map(board_item_key).collect();
    let selected = state.board_sel.resolve(&keys);
    let row = items
        .get(selected)
        .map(BoardRow::of)
        .unwrap_or(BoardRow::Inert);
    BoardSnapshot {
        keys,
        selected,
        row,
    }
}

/// The window of rows about to be drawn — formatted, and ONLY that window
/// (brief §10 mandate #7). Node formatted every row of every list every frame
/// and threw away the ones that did not fit.
pub struct BoardWindow {
    pub total: usize,
    pub selected: usize,
    /// Where the window starts, after scrolling just far enough to keep the
    /// cursor inside it.
    pub top: usize,
    pub lines: Vec<Line<'static>>,
}

pub fn window(state: &AppState, scroll_top: usize, height: usize) -> BoardWindow {
    let items = build_items(&state.board, &state.notifications);
    let keys: Vec<String> = items.iter().map(board_item_key).collect();
    let selected = state.board_sel.resolve(&keys);
    let top = crate::ui::components::keep_visible(selected, scroll_top, height, items.len());
    let live = live_task_ids(state.sessions());
    let ctx = state.board.ctx(&live);
    BoardWindow {
        total: items.len(),
        selected,
        top,
        lines: items
            .iter()
            .skip(top)
            .take(height)
            .map(|item| row_line(&format_board_item(item, &ctx)))
            .collect(),
    }
}

/// The pane's title, which carries the filter — the answer to "why is that task
/// missing".
pub fn label(board: &BoardSlice) -> String {
    let count = board
        .board
        .as_ref()
        .map(|b| b.task_count)
        .unwrap_or_default();
    format!(" Tasks Board · {} · {count} ", board.filter.as_str())
}
