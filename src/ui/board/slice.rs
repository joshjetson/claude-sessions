//! The board's half of the dashboard state.
//!
//! Kept out of [`crate::ui::state`] so neither file becomes the god object the
//! Node app's `state.js` was. Nothing here performs I/O: the board arrives as a
//! [`BoardUpdate`] from whichever feed is running (the daemon's `board` event,
//! or the in-process fetch) and everything else is derived from it.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::board::{BoardCtx, TaskSessionStatus};
use crate::daemon::{BoardFilter, TaskLink, TaskLinkStatus};
use crate::types::{Board, Session, Task};

/// One board fetch, however it was produced.
///
/// The fields mirror [`crate::daemon::Snapshot`]'s board half exactly, so the
/// daemon client maps one onto the other without interpreting anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardUpdate {
    pub board: Option<Board>,
    pub error: Option<String>,
    pub loading: bool,
    pub filter: BoardFilter,
    pub task_sessions: BTreeMap<i64, TaskLink>,
    pub done_tasks: Vec<i64>,
    pub archived_tasks: Vec<i64>,
    pub blocked_tasks: BTreeMap<i64, Vec<String>>,
    /// Task id -> recorded Optics processes, when this install has Optics
    /// configured. Empty is the normal case and means the 🔬 badges are off.
    pub optics_tasks: HashMap<i64, usize>,
}

impl BoardUpdate {
    /// A fetch that is still in flight. Distinct from "no board": the tab says
    /// "Loading tasks from Odoo…" rather than looking empty.
    pub fn loading(filter: BoardFilter) -> Self {
        BoardUpdate {
            loading: true,
            filter,
            ..BoardUpdate::default()
        }
    }

    pub fn failed(filter: BoardFilter, error: impl Into<String>) -> Self {
        BoardUpdate {
            error: Some(error.into()),
            filter,
            ..BoardUpdate::default()
        }
    }

    pub fn loaded(filter: BoardFilter, board: Board) -> Self {
        BoardUpdate {
            board: Some(board),
            filter,
            ..BoardUpdate::default()
        }
    }
}

/// What the right-hand pane is showing. Rows rather than strings so the detail
/// pane is styled by the same [`crate::board::Role`] table as the list, and so
/// a test can assert on its text without parsing escape sequences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardDetail {
    pub label: String,
    pub rows: Vec<crate::board::Row>,
    /// The task this pane is about, so an Odoo description that arrives after
    /// the cursor moved on is dropped rather than painted over another row.
    pub task_id: Option<i64>,
}

/// Everything the board tab draws from.
#[derive(Debug, Default)]
pub struct BoardSlice {
    pub board: Option<Board>,
    pub error: Option<String>,
    pub loading: bool,
    pub filter: BoardFilter,
    /// Project / stage / subtask keys that are open. Survives a refresh, which
    /// is what keeps the tree from collapsing under you every 45 seconds.
    pub expanded: HashSet<String>,
    /// Task id -> the session working it, as the daemon believes it.
    pub links: BTreeMap<i64, TaskLink>,
    pub done_tasks: HashSet<i64>,
    pub archived_tasks: HashSet<i64>,
    /// Task id -> the questions the readiness gate wants answered.
    pub blocked_tasks: BTreeMap<i64, Vec<String>>,
    /// Task id -> recorded Optics processes, behind the 🔬N badge.
    pub optics_tasks: HashMap<i64, usize>,
    pub detail: Option<BoardDetail>,
    /// The blink tick. Unread notifications flash on it.
    pub blink_on: bool,
    /// Derived when an update lands, because the row formatter wants a plain
    /// status per task and a set of gated ids — rebuilding either per row would
    /// be a map copy per frame.
    session_status: HashMap<i64, TaskSessionStatus>,
    blocked_ids: HashSet<i64>,
}

impl BoardSlice {
    /// Fold an update in. The expansion set and the cursor deliberately survive
    /// it — a board that re-collapsed itself on every poll would be unusable.
    pub fn apply(&mut self, update: BoardUpdate) {
        if update.loading && update.board.is_none() && update.error.is_none() {
            self.loading = true;
            return;
        }
        self.loading = false;
        self.error = update.error;
        if update.board.is_some() {
            self.board = update.board;
        }
        self.filter = update.filter;
        self.links = update.task_sessions;
        self.done_tasks = update.done_tasks.into_iter().collect();
        self.archived_tasks = update.archived_tasks.into_iter().collect();
        self.blocked_ids = update.blocked_tasks.keys().copied().collect();
        self.blocked_tasks = update.blocked_tasks;
        // Coverage is best-effort and arrives with the board it describes; an
        // empty map from a failed lookup must not wipe what is on screen.
        if !update.optics_tasks.is_empty() {
            self.optics_tasks = update.optics_tasks;
        }
        self.session_status = self
            .links
            .iter()
            .filter_map(|(task_id, link)| Some((*task_id, status_of(link.status?))))
            .collect();
        // A first load opens every project, so the tab is not a list of
        // collapsed headers the first time you press Tab.
        if self.expanded.is_empty() {
            if let Some(board) = &self.board {
                for name in board.projects.keys() {
                    self.expanded.insert(crate::board::project_key(name));
                }
            }
        }
    }

    /// Changing the filter throws the board away: the old one is the *other*
    /// filter's answer, and showing it while the new one loads displays exactly
    /// the tasks that were just filtered out.
    pub fn set_filter(&mut self, filter: BoardFilter) {
        self.filter = filter;
        self.board = None;
        self.error = None;
        self.expanded.clear();
    }

    pub fn toggle(&mut self, key: &str) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_string());
        }
    }

    pub fn set_expanded(&mut self, key: &str, expanded: bool) {
        if expanded {
            self.expanded.insert(key.to_string());
        } else {
            self.expanded.remove(key);
        }
    }

    /// The board row for a task id — including subtasks, which hang off their
    /// parent rather than sitting in a stage. A map would be a third copy of
    /// the board; the board is small and this is called on a keystroke, not a
    /// frame.
    pub fn task(&self, task_id: i64) -> Option<&Task> {
        let board = self.board.as_ref()?;
        board.projects.values().find_map(|project| {
            project.stages.values().find_map(|stage| {
                stage.tasks.iter().find_map(|task| {
                    if task.id == task_id {
                        return Some(task);
                    }
                    task.subtasks.iter().find(|sub| sub.id == task_id)
                })
            })
        })
    }

    /// Task id -> stage name for everything the loaded board covers, subtasks
    /// included. The purge dialog starts from this and only asks Odoo about
    /// what is missing — the board holds just what the current filter shows.
    pub fn stages(&self) -> BTreeMap<i64, String> {
        let mut out = BTreeMap::new();
        let Some(board) = &self.board else {
            return out;
        };
        for project in board.projects.values() {
            for (stage_name, stage) in &project.stages {
                for task in &stage.tasks {
                    out.insert(task.id, stage_name.clone());
                    for subtask in &task.subtasks {
                        let name = if subtask.stage_name.is_empty() {
                            stage_name.clone()
                        } else {
                            subtask.stage_name.clone()
                        };
                        out.insert(subtask.id, name);
                    }
                }
            }
        }
        out
    }

    pub fn link(&self, task_id: i64) -> Option<&TaskLink> {
        self.links.get(&task_id)
    }

    /// Everything the row formatter needs. `live` is the set of task ids a live
    /// transcript claims, which is what makes the running marker right after a
    /// daemon restart — the in-memory links are empty then.
    pub fn ctx<'a>(&'a self, live: &'a HashSet<i64>) -> BoardCtx<'a> {
        BoardCtx {
            task_sessions: Some(&self.session_status),
            done_tasks: Some(&self.done_tasks),
            archived_tasks: Some(&self.archived_tasks),
            blocked_tasks: Some(&self.blocked_ids),
            optics_tasks: Some(&self.optics_tasks),
            live_task_ids: Some(live),
            blink_on: self.blink_on,
            // The board draws the marker; [`crate::autodev`] decides what it
            // is from the task's Odoo tags.
            auto_dev: Some(crate::autodev::board_marker),
            now: None,
        }
    }
}

/// The task ids a live transcript names. Derived from the sessions rather than
/// from the launch links on purpose: a session started outside the dashboard,
/// or before the daemon restarted, is still working its task.
pub fn live_task_ids<'a>(sessions: impl Iterator<Item = &'a Session>) -> HashSet<i64> {
    sessions.filter_map(|session| session.task_id).collect()
}

fn status_of(status: TaskLinkStatus) -> TaskSessionStatus {
    match status {
        TaskLinkStatus::Done => TaskSessionStatus::Done,
        _ => TaskSessionStatus::Running,
    }
}
