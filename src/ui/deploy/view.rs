//! The Deploy tab's list and its cursor.
//!
//! The rows themselves are built by [`crate::board::deploy`], which is pure and
//! already tested; this is the part that knows where they come from, which one
//! the cursor is on, and what stands in for the tree while there is none.

use ratatui::text::Line;

use crate::board::{build_deploy_tree, deploy_item_key, format_deploy_item, DeployItem};
use crate::types::DeployTask;
use crate::ui::spans::row_line;
use crate::ui::state::AppState;

use super::slice::DeploySlice;

/// Messages that stand in for the tree. The Deploy tab refreshes ONLY when
/// asked, so "press r" is not a nag — it is the whole interface.
const LOADING: &str = "Loading the deploy board…";
const RETRY: &str = "Press r to retry";

pub fn build_items(deploy: &DeploySlice) -> Vec<DeployItem<'_>> {
    match (&deploy.board, &deploy.error) {
        (_, Some(error)) => vec![
            DeployItem::Info {
                name: error,
                project_name: "",
            },
            DeployItem::Info {
                name: RETRY,
                project_name: "",
            },
        ],
        (None, None) if deploy.loading => vec![DeployItem::Info {
            name: LOADING,
            project_name: "",
        }],
        (None, None) => vec![DeployItem::Info {
            name: "Press r to load the deploy board.",
            project_name: "",
        }],
        (Some(board), None) => build_deploy_tree(board, &deploy.expanded, &deploy.runs),
    }
}

/// The row the cursor is on, owned so the caller can mutate state afterwards.
#[derive(Debug, Clone, PartialEq)]
pub enum DeployRow {
    Project {
        name: String,
    },
    Task {
        task: Box<DeployTask>,
        project: String,
    },
    /// A message — nothing to act on.
    Inert,
}

impl DeployRow {
    fn of(item: &DeployItem<'_>) -> DeployRow {
        match item {
            DeployItem::Project { name, .. } => DeployRow::Project {
                name: (*name).to_string(),
            },
            DeployItem::Task { task, project_name } => DeployRow::Task {
                task: Box::new((*task).clone()),
                project: (*project_name).to_string(),
            },
            DeployItem::Info { .. } => DeployRow::Inert,
        }
    }

    pub fn task(&self) -> Option<&DeployTask> {
        match self {
            DeployRow::Task { task, .. } => Some(task),
            _ => None,
        }
    }

    /// The project this row belongs to. A task row answers for its project, so
    /// `d`, `M` and `c` work from anywhere in a project's block without moving
    /// the cursor back to the header.
    pub fn project_name(&self) -> Option<&str> {
        match self {
            DeployRow::Project { name } => Some(name),
            DeployRow::Task { project, .. } => Some(project),
            DeployRow::Inert => None,
        }
    }

    pub fn expand_key(&self) -> Option<String> {
        match self {
            DeployRow::Project { name } => Some(crate::board::deploy_project_key(name)),
            _ => None,
        }
    }
}

pub struct DeploySnapshot {
    pub keys: Vec<String>,
    pub selected: usize,
    pub row: DeployRow,
}

pub fn snapshot(state: &AppState) -> DeploySnapshot {
    let items = build_items(&state.deploy);
    let keys: Vec<String> = items.iter().map(deploy_item_key).collect();
    let selected = state.deploy_sel.resolve(&keys);
    let row = items
        .get(selected)
        .map(DeployRow::of)
        .unwrap_or(DeployRow::Inert);
    DeploySnapshot {
        keys,
        selected,
        row,
    }
}

/// The window of rows about to be drawn — formatted, and ONLY that window
/// (brief §10 mandate #7).
pub struct DeployWindow {
    pub total: usize,
    pub selected: usize,
    pub top: usize,
    pub lines: Vec<Line<'static>>,
}

pub fn window(state: &AppState, scroll_top: usize, height: usize) -> DeployWindow {
    let items = build_items(&state.deploy);
    let keys: Vec<String> = items.iter().map(deploy_item_key).collect();
    let selected = state.deploy_sel.resolve(&keys);
    let top = crate::ui::components::keep_visible(selected, scroll_top, height, items.len());
    let now = chrono::Local::now();
    DeployWindow {
        total: items.len(),
        selected,
        top,
        lines: items
            .iter()
            .skip(top)
            .take(height)
            .map(|item| row_line(&format_deploy_item(item, now)))
            .collect(),
    }
}

/// The pane's title. The count is of OUTSTANDING tasks, because zero is the
/// answer the tab exists to give.
pub fn label(deploy: &DeploySlice) -> String {
    let outstanding: usize = deploy
        .board
        .as_ref()
        .map(|board| board.projects.values().map(|p| p.tasks.len()).sum())
        .unwrap_or_default();
    let running = deploy.running_deploys().len();
    if running > 0 {
        return format!(" Deploy · {outstanding} outstanding · {running} running ");
    }
    format!(" Deploy · {outstanding} outstanding ")
}
