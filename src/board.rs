//! The task board as a flat list of rows: what to draw, and how each row reads.
//!
//! Everything here is pure. The tree is built from a [`Board`] plus the set of
//! expanded keys, and a row is formatted from an item plus a [`BoardCtx`] of
//! everything the daemon knows about that task. Nothing reads the clock or the
//! filesystem, so the whole module is testable without a running dashboard.
//!
//! Items borrow from the board rather than cloning it: the tree is rebuilt
//! whenever the board or the expansion set changes, and a task with subtasks is
//! not cheap to copy.

mod deploy;
mod format;
mod row;

pub use deploy::{
    build_deploy_tree, deploy_item_key, deploy_project_key, format_deploy_item, DeployItem,
};
pub use format::{format_board_item, AutoDevResolver, AutoMarker, BoardCtx, TaskSessionStatus};
pub use row::{plain_text, Role, Row, RowBuilder, Segment, Style};

use std::collections::HashSet;

use crate::types::{Board, Notification, Task};

pub fn project_key(name: &str) -> String {
    format!("bp:{name}")
}

pub fn stage_key(project_name: &str, stage_name: &str) -> String {
    format!("bs:{project_name}:{stage_name}")
}

pub fn subtask_key(task_id: i64) -> String {
    format!("bsub:{task_id}")
}

/// One row of the board.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardItem<'a> {
    Project {
        name: &'a str,
        project_id: i64,
        task_count: usize,
        expanded: bool,
    },
    Stage {
        project_name: &'a str,
        stage_name: &'a str,
        stage_id: i64,
        count: usize,
        expanded: bool,
    },
    Task {
        task: &'a Task,
        project_name: &'a str,
        stage_name: &'a str,
        sub_count: usize,
        sub_expanded: bool,
    },
    Subtask {
        task: &'a Task,
        parent_id: i64,
    },
    /// A message where rows would be — "no credentials", "nothing assigned".
    Info {
        name: &'a str,
    },
    /// Blank line between the notification feed and the board.
    Separator,
    NotificationHeader {
        unread: usize,
        total: usize,
    },
    Notification {
        notif: &'a Notification,
    },
}

/// The key a row is selected and expanded by. Stable across refreshes, which is
/// what keeps the cursor where you left it when the board reloads.
pub fn board_item_key(item: &BoardItem<'_>) -> String {
    match item {
        BoardItem::Project { name, .. } => project_key(name),
        BoardItem::Stage {
            project_name,
            stage_name,
            ..
        } => stage_key(project_name, stage_name),
        BoardItem::Task { task, .. } => format!("bt:{}", task.id),
        BoardItem::Subtask { task, .. } => format!("st:{}", task.id),
        BoardItem::Info { name } => format!("info:{name}"),
        BoardItem::Notification { notif } => format!("n:{}", notif.id),
        BoardItem::NotificationHeader { .. } => "nhdr".to_string(),
        BoardItem::Separator => "nsep".to_string(),
    }
}

/// Flatten the board into rows, honouring the expansion state.
///
/// Projects come out in name order; stages by their Odoo sequence, then name,
/// so two projects that share a stage list agree on the order; tasks in the
/// order the query returned them (most recently touched first).
pub fn build_board_tree<'a>(board: &'a Board, expanded: &HashSet<String>) -> Vec<BoardItem<'a>> {
    let mut items = Vec::new();

    for (name, project) in &board.projects {
        let task_count: usize = project.stages.values().map(|stage| stage.tasks.len()).sum();
        let project_expanded = expanded.contains(&project_key(name));
        items.push(BoardItem::Project {
            name,
            project_id: project.project_id,
            task_count,
            expanded: project_expanded,
        });
        if !project_expanded {
            continue;
        }

        let mut stages: Vec<_> = project.stages.iter().collect();
        stages.sort_by(|(a_name, a), (b_name, b)| {
            a.sequence.cmp(&b.sequence).then_with(|| a_name.cmp(b_name))
        });

        for (stage_name, stage) in stages {
            let stage_expanded = expanded.contains(&stage_key(name, stage_name));
            items.push(BoardItem::Stage {
                project_name: name,
                stage_name,
                stage_id: stage.stage_id,
                count: stage.tasks.len(),
                expanded: stage_expanded,
            });
            if !stage_expanded {
                continue;
            }

            for task in &stage.tasks {
                let sub_count = task.subtasks.len();
                let sub_expanded = sub_count > 0 && expanded.contains(&subtask_key(task.id));
                items.push(BoardItem::Task {
                    task,
                    project_name: name,
                    stage_name,
                    sub_count,
                    sub_expanded,
                });
                if sub_expanded {
                    for sub in &task.subtasks {
                        items.push(BoardItem::Subtask {
                            task: sub,
                            parent_id: task.id,
                        });
                    }
                }
            }
        }
    }

    items
}

#[cfg(test)]
mod tests;
