//! What the board tests build their world out of.
//!
//! The Node suites relied on `test/helpers/isolate.js`: a fresh
//! `CLAUDE_SESSIONS_HOME` per test, because `findTaskSessionAnywhere` reads
//! every transcript it can find. Here that is a `Paths` built on a `TempDir` and
//! passed in, so the isolation is structural rather than an env var every test
//! has to remember to set.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use crate::daemon::{TaskLink, TaskLinkStatus};
use crate::term::{DriverResult, LaunchRequest, SessionRef, TerminalDriver};
use crate::types::{
    Board, BoardProject, BoardStage, Notification, NotificationKind, NotificationLevel,
    NotificationStatus, Session, SessionStatus, Task,
};
use crate::ui::board::{BoardUpdate, StartRequest};
use crate::ui::state::{Action, AppState, View};
use crate::ui::tests::temp_state;

/// A state already on the board tab.
pub fn board_state() -> (TempDir, AppState) {
    let (dir, mut state) = temp_state();
    state.view = View::Board;
    (dir, state)
}

/// A task with only what the board needs. Deliberately in a project that maps
/// to no folder: a mapped project would send the launch flow down the real
/// launch path.
pub fn task(id: i64, name: &str) -> Task {
    Task {
        id,
        name: name.to_string(),
        stage_id: 1,
        stage_name: "Approved to Start".to_string(),
        project_id: 3,
        project_name: "NoSuchProject-ForTests".to_string(),
        ..Task::default()
    }
}

/// A task Odoo says is blocked by work that has not landed.
pub fn blocked_task(id: i64) -> Task {
    Task {
        blocked_by: vec![4034],
        blocker_count: 1,
        open_blocker_count: 1,
        ..task(id, "Per-project default report templates")
    }
}

/// A live session working a task, as the scanner reports one.
pub fn live_session(session_id: &str, task_id: Option<i64>, mtime_secs: u64) -> Session {
    Session {
        session_id: session_id.to_string(),
        pids: vec![4242],
        cwd: "/repo".to_string(),
        tty: Some("ttys004".to_string()),
        lstart: None,
        session_file: Some(std::path::PathBuf::from(format!("/x/{session_id}.jsonl"))),
        session_mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs),
        session_size: Some(1024),
        status: SessionStatus::Working,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id,
        run_id: None,
    }
}

/// Put sessions on the state, under one project.
pub fn with_sessions(state: &mut AppState, sessions: Vec<Session>) {
    let mut by_project = std::collections::BTreeMap::new();
    by_project.insert("repo".to_string(), sessions);
    state.apply_sessions(by_project);
    state.take_actions();
}

/// One project, one stage, the given tasks.
pub fn with_tasks(state: &mut AppState, tasks: Vec<Task>) {
    let project = tasks
        .first()
        .map(|t| t.project_name.clone())
        .unwrap_or_default();
    let stage = tasks
        .first()
        .map(|t| t.stage_name.clone())
        .unwrap_or_default();
    let mut board = Board {
        task_count: tasks.len(),
        ..Board::default()
    };
    board.projects.insert(
        project,
        BoardProject {
            project_id: 3,
            stages: [(
                stage,
                BoardStage {
                    stage_id: 1,
                    sequence: 1,
                    tasks,
                },
            )]
            .into_iter()
            .collect(),
        },
    );
    state.apply_board(BoardUpdate::loaded(crate::daemon::BoardFilter::Mine, board));
    state.take_actions();
}

/// Record that the dashboard launched a session for a task.
pub fn with_link(state: &mut AppState, task_id: i64, link: TaskLink) {
    state.board.links.insert(task_id, link);
}

pub fn running_link(cwd: &str, session_id: &str) -> TaskLink {
    TaskLink {
        cwd: cwd.to_string(),
        session_id: session_id.to_string(),
        session_file: None,
        status: Some(TaskLinkStatus::Running),
        stage_id: None,
    }
}

pub fn notification(id: &str, task_id: Option<i64>) -> Notification {
    Notification {
        id: id.to_string(),
        title: "needs a decision".to_string(),
        message: "which behaviour should win?".to_string(),
        cwd: "/repo".to_string(),
        project: "repo".to_string(),
        session_id: None,
        task_id,
        level: NotificationLevel::Warn,
        kind: NotificationKind::Info,
        ts: "2026-09-16T14:05:06.000Z".to_string(),
        status: NotificationStatus::Unread,
    }
}

/// A start request for a task, with nothing typed and nothing forced.
pub fn start_request(task: &Task, kind: crate::ui::board::LaunchKind) -> StartRequest {
    StartRequest::new(task, kind)
}

/// The queue, drained. Most board assertions are about what was enqueued rather
/// than about what happened — nothing happens on this thread.
pub fn actions(state: &mut AppState) -> Vec<Action> {
    state.take_actions()
}

/// A terminal driver that records instead of driving.
///
/// The anti-spawn guard is [`crate::term::SpawnPolicy`]; this is the second
/// layer, so even a test that deliberately passes `Allow` to exercise the write
/// path cannot open a terminal.
#[derive(Debug, Default)]
pub struct RecordingDriver {
    pub launches: Mutex<Vec<LaunchRequest>>,
    pub sent: Mutex<Vec<(SessionRef, String)>>,
    pub focused: AtomicUsize,
    /// Tabs the driver was asked to close — what a purge must not reach under
    /// a refusing spawn policy.
    pub closed: AtomicUsize,
    pub fail: bool,
}

impl RecordingDriver {
    pub fn shared() -> Arc<RecordingDriver> {
        Arc::new(RecordingDriver::default())
    }

    pub fn launched(&self) -> Vec<LaunchRequest> {
        self.launches.lock().unwrap().clone()
    }

    pub fn sends(&self) -> Vec<(SessionRef, String)> {
        self.sent.lock().unwrap().clone()
    }

    pub fn focus_count(&self) -> usize {
        self.focused.load(Ordering::SeqCst)
    }
}

impl TerminalDriver for RecordingDriver {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn launch(&self, request: &LaunchRequest) -> DriverResult {
        self.launches.lock().unwrap().push(request.clone());
        if self.fail {
            DriverResult::failed("recording driver was told to fail")
        } else {
            DriverResult::ok()
        }
    }

    fn send_text(&self, session: &SessionRef, text: &str) -> DriverResult {
        self.sent
            .lock()
            .unwrap()
            .push((session.clone(), text.to_string()));
        DriverResult::ok()
    }

    fn focus(&self, _session: &SessionRef) -> DriverResult {
        self.focused.fetch_add(1, Ordering::SeqCst);
        DriverResult::ok()
    }

    fn close(&self, _session: &SessionRef) -> DriverResult {
        self.closed.fetch_add(1, Ordering::SeqCst);
        DriverResult::ok()
    }
}
