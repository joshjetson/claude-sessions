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
            // Ordered by when each session STARTED, oldest first.
            //
            // This used to sort by last activity, so whichever agent wrote most
            // recently jumped to the top of its project and every row below it
            // shifted down. With several agents working, the list reordered
            // about once a second: you could not point at a row, and a keypress
            // could land on a different session than the one you aimed at.
            //
            // Start time never changes while a session lives, so a row stays
            // where it is for as long as it exists. Ties break on session id,
            // which is stable too — two sessions can share a start second.
            sessions.sort_by(|a, b| {
                started_at(a)
                    .cmp(&started_at(b))
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
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
/// When a session STARTED, which is what the tree orders on.
///
/// `lstart` is what `ps` reports and never changes while the process lives, so
/// a row holds its position. The transcript's mtime is the fallback for a
/// session whose start time could not be read — imperfect, but it at least does
/// not move every time the agent writes.
/// When a session's process started, for the ordering above.
///
/// `start_time_instant`, NOT `parse_timestamp`. `lstart` is whatever `ps`
/// printed — `Thu Sep 17 21:29:46 2026` — and `parse_timestamp` reads RFC3339
/// only, so it returned `None` for every session ever passed to it. The sort
/// then fell through to the transcript mtime, which is the activity ordering
/// this function exists to replace: the fix was present and inert, and the
/// list reordered on every write exactly as it had before.
///
/// `start_time_instant` parses the `ps` form and RFC3339 both, so neither
/// source of a start time is left out.
///
/// The mtime fallback stays for the case it was meant for: a transcript with no
/// live process has no `lstart` at all.
fn started_at(session: &Session) -> SystemTime {
    session
        .lstart
        .as_deref()
        .and_then(crate::util::start_time_instant)
        .unwrap_or(session.session_mtime)
}

/// Everything the engine owns between ticks.
#[derive(Debug, Default)]
pub struct EngineState {
    pub sessions: SessionIndex,
    /// The tick that produced [`Self::sessions`] saw everything it looked for,
    /// so an empty index there is a machine with no sessions and not a failed
    /// read. `false` until the first tick, which is the safe answer.
    pub sessions_complete: bool,
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
    /// The QA role's one aggregated quiet-sessions row.
    pub quiet: QuietSessions,
    /// Set when a slow tick skipped the QA arrival watcher because the role was
    /// not QA. The next QA tick then records what is in the stages silently,
    /// so switching role does not announce every task that arrived meanwhile.
    pub qa_watch_paused: bool,

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

/// Where the quiet-sessions row stands.
///
/// The row is one notification with a fixed id, updated in place. What this
/// tracks is the part that is not in the row itself: which sessions it
/// describes, and what the user has already dismissed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuietSessions {
    /// The session ids quiet as of the last tick.
    pub current: BTreeSet<String>,
    /// Whether the row is in the feed and the daemon keeps it up to date.
    pub shown: bool,
    /// The quiet set at the moment the user dismissed, resolved or cleared the
    /// row. While every quiet session is one of these, the row stays hidden. A
    /// session that starts writing again leaves this set, so its next silence
    /// counts as new.
    pub dismissed: Option<BTreeSet<String>>,
}

/// What the quiet-sessions row should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietStep {
    /// Nothing to show, or the user dismissed everything that is quiet.
    Stay,
    /// First appearance, or a new session went quiet after a dismissal. The
    /// only step that plays a sound.
    Raise,
    /// The row is up. Refresh its text in place, silently.
    Update,
    /// No session is quiet any more. Take the row away.
    Remove,
}

impl QuietSessions {
    /// Decide this tick's step from the sessions quiet right now, and record
    /// them.
    pub fn plan(&mut self, quiet: BTreeSet<String>) -> QuietStep {
        if let Some(dismissed) = &mut self.dismissed {
            dismissed.retain(|id| quiet.contains(id));
        }
        let step = if quiet.is_empty() {
            self.dismissed = None;
            if self.shown {
                QuietStep::Remove
            } else {
                QuietStep::Stay
            }
        } else if let Some(dismissed) = &self.dismissed {
            if quiet.is_subset(dismissed) {
                QuietStep::Stay
            } else {
                QuietStep::Raise
            }
        } else if self.shown {
            QuietStep::Update
        } else {
            QuietStep::Raise
        };
        match step {
            QuietStep::Raise => {
                self.dismissed = None;
                self.shown = true;
            }
            QuietStep::Remove => self.shown = false,
            QuietStep::Stay | QuietStep::Update => {}
        }
        self.current = quiet;
        step
    }

    /// The user dismissed, resolved or cleared the row.
    pub fn dismiss(&mut self) {
        self.dismissed = Some(self.current.clone());
        self.shown = false;
    }

    /// The role stopped wanting the row. Forget everything, so switching back
    /// raises it fresh.
    pub fn reset(&mut self) -> bool {
        let was_shown = self.shown;
        *self = QuietSessions::default();
        was_shown
    }
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
