//! Ported from the row-formatting behaviour of the Node app's `src/board.js`,
//! plus the board-marker assertions in its `test/dependencies.test.js`.
//!
//! The rows are compared as plain text — the exact spacing is the contract the
//! panes are laid out against — and separately as styles, because "which glyph"
//! and "what colour" are two different regressions.

mod deploy;
mod format;
mod tree;

use std::collections::BTreeMap;

use chrono::{DateTime, Local, TimeZone};

use super::{plain_text, Role, Row};
use crate::types::{Board, BoardProject, BoardStage, Task};

/// A fixed instant, so "overdue" never depends on when the suite runs.
pub(crate) fn now() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap()
}

pub(crate) fn task(id: i64, name: &str) -> Task {
    Task {
        id,
        name: name.to_string(),
        stage_name: "In Progress".to_string(),
        project_name: "Beacon".to_string(),
        project_id: 3,
        stage_id: 2,
        ..Task::default()
    }
}

/// A one-project board: `Beacon` with the given stages, each named with its
/// sequence and its tasks.
pub(crate) fn board(stages: Vec<(&str, i64, Vec<Task>)>) -> Board {
    let mut by_name: BTreeMap<String, BoardStage> = BTreeMap::new();
    for (name, sequence, tasks) in stages {
        by_name.insert(
            name.to_string(),
            BoardStage {
                stage_id: sequence,
                sequence,
                tasks,
            },
        );
    }
    let mut projects = BTreeMap::new();
    projects.insert(
        "Beacon".to_string(),
        BoardProject {
            project_id: 3,
            stages: by_name,
        },
    );
    Board {
        task_count: projects
            .values()
            .flat_map(|project| project.stages.values())
            .map(|stage| stage.tasks.len())
            .sum(),
        projects,
        truncated: false,
    }
}

/// The styles a row carries, for the glyphs that matter.
pub(crate) fn role_of(row: &Row, text: &str) -> Option<Role> {
    row.iter()
        .find(|segment| segment.text.contains(text))
        .map(|segment| segment.style.role)
}

pub(crate) fn text(row: &Row) -> String {
    plain_text(row)
}
