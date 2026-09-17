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

use std::collections::{HashMap, HashSet};

use crate::qarun::{
    build_run_entries, run_key, run_summary, run_task_key, QaRun, RunCtx, RunEntry, RunSummary,
};
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

/// A run is expanded by default, so the expansion set records the opposite.
pub fn collapsed_key(run_id: &str) -> String {
    format!("{}:collapsed", run_key(run_id))
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
    /// The header of a QA run, pinned as the first child of the stage it covers.
    QaRun {
        run: &'a QaRun,
        summary: RunSummary,
        expanded: bool,
    },
    /// One task inside a run. Still a task row — every key that works on
    /// [`BoardItem::Task`] works here, which is most of the argument for
    /// putting runs on the board rather than in a tab of their own.
    QaRunTask {
        entry: RunEntry<'a>,
        run: &'a QaRun,
        /// `None` when the board no longer carries the task: it failed QA and
        /// left the stage, but the run still owns it.
        task: Option<&'a Task>,
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
        BoardItem::QaRun { run, .. } => run_key(&run.id),
        BoardItem::QaRunTask { run, entry, .. } => run_task_key(&run.id, entry.task_id),
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
    build_board_tree_with_runs(board, expanded, &[], None)
}

/// The board, with any QA runs folded into the stages they cover.
///
/// A run leads its stage: it is the thing the stage was opened to look at. Its
/// tasks are claimed, so they do not also appear below as loose stage rows —
/// duplicated rows would double every count the reader makes. A collapsed run
/// still claims them, because a run releasing its tasks on collapse would read
/// as the run having given them up.
pub fn build_board_tree_with_runs<'a>(
    board: &'a Board,
    expanded: &HashSet<String>,
    runs: &'a [QaRun],
    // `None` when there are no runs. A context is only ever needed to decide
    // what a run's rows say, so requiring one to draw a board without runs
    // would mean manufacturing an empty one at every call site.
    run_ctx: Option<&'a RunCtx<'a>>,
) -> Vec<BoardItem<'a>> {
    let mut items = Vec::new();
    // At most one run per stage. Starting a second over the same tasks is the
    // collision the one-pass-per-task rule already refuses.
    let runs_by_stage: HashMap<(&str, &str), &QaRun> = runs
        .iter()
        .map(|run| {
            (
                (run.project_name.as_str(), run.stage_name.as_str()),
                run,
            )
        })
        .collect();

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
            // Case-insensitive on the tie-break, because Node compared with
            // `localeCompare` (`board.js:37`): byte order puts every
            // capitalised stage ahead of every lowercase one, so two stages
            // sharing a sequence came out in the opposite order.
            a.sequence.cmp(&b.sequence).then_with(|| {
                a_name
                    .to_lowercase()
                    .cmp(&b_name.to_lowercase())
                    .then_with(|| a_name.cmp(b_name))
            })
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

            let mut claimed: HashSet<i64> = HashSet::new();
            if let (Some(run), Some(run_ctx)) = (
                runs_by_stage.get(&(name.as_str(), stage_name.as_str())),
                run_ctx,
            ) {
                let entries = build_run_entries(run, run_ctx);
                let summary = run_summary(&entries);
                // Stored inverted, so a brand-new run renders open: the point of
                // creating one is to look at it.
                let run_expanded = !expanded.contains(&collapsed_key(&run.id));
                items.push(BoardItem::QaRun {
                    run,
                    summary,
                    expanded: run_expanded,
                });
                for entry in entries {
                    claimed.insert(entry.task_id);
                    if run_expanded {
                        let task = stage.tasks.iter().find(|t| t.id == entry.task_id);
                        items.push(BoardItem::QaRunTask { entry, run, task });
                    }
                }
            }

            for task in &stage.tasks {
                if claimed.contains(&task.id) {
                    continue;
                }
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
