//! What the engine knows between ticks.
//!
//! The Node app kept this in `src/state.js` as a module-global mutable
//! singleton that the engine and the TUI both reached into — the biggest
//! structural wart in the codebase, and the reason a test could only run one
//! engine at a time. Here it is an owned value behind one mutex, and the TUI
//! gets a [`super::Snapshot`] instead of a reference.
//!
//! The per-refresh indexes at the bottom are Big-O mandate #10: "which session
//! is this id" and "which task is this id" are map lookups, never the nested
//! scan through projects → stages → tasks that `findTaskById` was.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::types::{Board, DeployBoard, Notification, Session, Task};

/// How long a queued launch waits for its session before it is given up on —
/// the same window the fast poll runs for.
pub(crate) const PENDING_WINDOW: Duration = Duration::from_secs(30);

/// Which tasks the board shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardFilter {
    #[default]
    Mine,
    All,
}

impl BoardFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            BoardFilter::Mine => "mine",
            BoardFilter::All => "all",
        }
    }

    /// `None` for anything else: a filter arriving over HTTP is user input, and
    /// an unrecognised value leaves the board as it was.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "mine" => Some(BoardFilter::Mine),
            "all" => Some(BoardFilter::All),
            _ => None,
        }
    }
}

/// How far along a task's session is, as the engine believes it.
///
/// Only [`TaskLinkStatus::Running`] links are watched for stalls — a task that
/// finished, was archived or hit the readiness gate is not stalled, it is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskLinkStatus {
    Running,
    Ended,
    Done,
    Blocked,
}

/// The session working a task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLink {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskLinkStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id: Option<i64>,
}

impl TaskLink {
    pub fn is_running(&self) -> bool {
        self.status == Some(TaskLinkStatus::Running)
    }
}

/// A partial update to a link. Every field is optional because callers know
/// different halves of it: the launch flow knows the directory and the stage,
/// the linker knows the session, the completion path knows the status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskLinkPatch {
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub session_file: Option<PathBuf>,
    pub status: Option<TaskLinkStatus>,
    pub stage_id: Option<i64>,
}

impl TaskLinkPatch {
    fn apply(self, link: &mut TaskLink) {
        if let Some(cwd) = self.cwd {
            link.cwd = cwd;
        }
        if let Some(session_id) = self.session_id {
            link.session_id = session_id;
        }
        if self.session_file.is_some() {
            link.session_file = self.session_file;
        }
        if self.status.is_some() {
            link.status = self.status;
        }
        if self.stage_id.is_some() {
            link.stage_id = self.stage_id;
        }
    }
}

/// A task the readiness gate stopped, with the questions it wants answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedTask {
    pub questions: Vec<String>,
    pub ts: String,
}

/// One launch still waiting for its session to appear.
///
/// One entry per task, never one slot: eight tasks started inside a minute all
/// raced for the single slot the Node app kept, and seven of them ended up
/// pointing at one placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLaunch {
    pub cwd: String,
    pub task_id: Option<i64>,
    pub launched_at: SystemTime,
    /// Session ids that already existed when the launch went out, so the new
    /// one can be told apart from them.
    pub known_session_ids: HashSet<String>,
}

/// The enriched sessions of one tick, grouped for the tree and indexed by id.
#[derive(Debug, Clone, Default)]
pub struct SessionIndex {
    by_project: BTreeMap<String, Vec<Session>>,
    /// session id -> (project, position). Mandate #10: a lookup, never a scan.
    by_id: HashMap<String, (String, usize)>,
}

impl SessionIndex {
    /// Group by project name, newest activity first inside each, and index.
    pub fn build(sessions: Vec<Session>, project_of: impl Fn(&Session) -> String) -> SessionIndex {
        let mut by_project: BTreeMap<String, Vec<Session>> = BTreeMap::new();
        for session in sessions {
            by_project
                .entry(project_of(&session))
                .or_default()
                .push(session);
        }
        let mut by_id = HashMap::new();
        for (project, sessions) in &mut by_project {
            // Newest first: the transcript's own last timestamp where it has
            // one, the file's mtime where it does not.
            sessions.sort_by_key(|session| std::cmp::Reverse(activity_at(session)));
            for (index, session) in sessions.iter().enumerate() {
                by_id.insert(session.session_id.clone(), (project.clone(), index));
            }
        }
        SessionIndex { by_project, by_id }
    }

    pub fn get(&self, session_id: &str) -> Option<&Session> {
        let (project, index) = self.by_id.get(session_id)?;
        self.by_project.get(project)?.get(*index)
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.by_id.contains_key(session_id)
    }

    pub fn by_project(&self) -> &BTreeMap<String, Vec<Session>> {
        &self.by_project
    }

    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.by_project.values().flatten()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn project_count(&self) -> usize {
        self.by_project.len()
    }
}

/// A transcript's own last timestamp, falling back to the file's mtime.
fn activity_at(session: &Session) -> SystemTime {
    session
        .last_timestamp
        .as_deref()
        .and_then(crate::util::parse_timestamp)
        .map(SystemTime::from)
        .unwrap_or(session.session_mtime)
}

/// Everything the engine owns between ticks.
#[derive(Debug, Default)]
pub struct EngineState {
    pub sessions: SessionIndex,
    pub task_sessions: BTreeMap<i64, TaskLink>,
    pub done_tasks: BTreeSet<i64>,
    pub blocked_tasks: BTreeMap<i64, BlockedTask>,
    pub archived_tasks: BTreeSet<i64>,
    /// Sessions already announced as waiting on a decision — the edge that
    /// keeps the notification from repeating every second.
    pub await_notified: HashSet<String>,
    pub pending: Vec<PendingLaunch>,
    /// Newest first, capped at [`super::NOTIFICATION_LIMIT`] (mandate #14: a
    /// deque, not `unshift` + `truncate`).
    pub notifications: VecDeque<Notification>,
    /// Group path -> the repository folders inside it.
    pub discovered_dirs: BTreeMap<String, Vec<String>>,
    /// Task id -> when it was last flagged as silent.
    pub stall_alerted_at: HashMap<i64, SystemTime>,
    /// session id -> the task it is working, as of the last tick. What makes
    /// archiving a session that VANISHED possible at all.
    pub seen_task_sessions: HashMap<String, SeenTaskSession>,

    // --- fed by later phases, carried by this one ---------------------------
    /// Phase 9b fills this; the engine only indexes it and hands it out.
    pub board: Option<Board>,
    pub board_error: Option<String>,
    pub board_loading: bool,
    pub board_filter: BoardFilter,
    /// Task id -> the board row, rebuilt whenever the board is replaced.
    /// `find_task_by_id` was a nested scan over projects and stages in Node,
    /// run on every completion and every stall alert.
    tasks_by_id: HashMap<i64, Task>,
    /// Phase 11 defines the shape; the engine carries it to clients unopened.
    pub usage: Option<serde_json::Value>,
    /// The Deploy tab. MANUAL only — nothing polls it, because every refresh
    /// spends a GitLab API call per open merge request.
    pub deploy: Option<DeployBoard>,
    pub deploy_error: Option<String>,
    /// Deploys this engine started and is supervising, keyed by the configured
    /// project name.
    pub deploy_runs: BTreeMap<String, super::deploy::DeployRunState>,
}

/// A live task session as the previous tick saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenTaskSession {
    pub task_id: i64,
    pub cwd: String,
    pub session_file: PathBuf,
}

impl EngineState {
    /// Merge a patch into a task's link, creating it if need be.
    pub fn link_task(&mut self, task_id: i64, patch: TaskLinkPatch) -> TaskLink {
        let link = self.task_sessions.entry(task_id).or_default();
        patch.apply(link);
        link.clone()
    }

    /// The board row for a task — one map hit (mandate #10).
    pub fn task(&self, task_id: i64) -> Option<&Task> {
        self.tasks_by_id.get(&task_id)
    }

    /// Replace the board and rebuild its index in one step, so the two can
    /// never disagree.
    pub fn set_board(&mut self, board: Option<Board>) {
        self.tasks_by_id.clear();
        if let Some(board) = &board {
            for project in board.projects.values() {
                for stage in project.stages.values() {
                    for task in &stage.tasks {
                        self.tasks_by_id.insert(task.id, task.clone());
                        for sub in &task.subtasks {
                            self.tasks_by_id.insert(sub.id, sub.clone());
                        }
                    }
                }
            }
        }
        self.board = board;
    }

    /// Drop launches that have waited longer than the fast-refresh window.
    pub fn prune_pending(&mut self, now: SystemTime) {
        self.pending.retain(|launch| {
            now.duration_since(launch.launched_at).unwrap_or_default() < PENDING_WINDOW
        });
    }

    /// Whether a launch is still in flight, which is what makes the poll run at
    /// twice its usual rate.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}
