//! What leaves the engine.
//!
//! One typed event per thing that happened, published to every subscriber over
//! a channel. Phase 6's server turns each into an SSE frame and Phase 7's TUI
//! reads the same channel directly, so the names below are the wire contract —
//! they are the Node app's `EVENTS` map verbatim.
//!
//! The bus is deliberately dumb: it fans out, drops subscribers whose receiver
//! has gone, and does nothing else. Coalescing and render gating belong to the
//! client that is drawing.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::state::{BlockedTask, EngineState, TaskLink};
use super::{lock, NOTIFICATION_LIMIT};
use crate::types::{Board, Notification, NotificationStatus, Session};

/// Sessions are sent to clients without their parsed transcript payload.
///
/// `last_entry`, `prompts` and `cumulative_usage` are large, change every
/// second and are already summarised into `status` and `activity_detail` before
/// they get here — shipping them meant re-serialising a few hundred kilobytes
/// of transcript per client per second to tell it nothing.
pub fn wire_session(session: &Session) -> Session {
    Session {
        last_entry: None,
        prompts: Vec::new(),
        cumulative_usage: None,
        ..session.clone()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub total_sessions: usize,
    pub total_projects: usize,
}

/// The payload of a `sessions` event: one tick's worth of everything the tree
/// draws.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsEvent {
    pub by_project: BTreeMap<String, Vec<Session>>,
    pub stats: SessionStats,
    pub discovered_dirs: BTreeMap<String, Vec<String>>,
}

/// Everything a client that just connected needs to be current.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub sessions: SessionsEvent,
    pub task_sessions: BTreeMap<i64, TaskLink>,
    pub archived_tasks: Vec<i64>,
    pub done_tasks: Vec<i64>,
    pub blocked_tasks: BTreeMap<i64, BlockedTask>,
    pub notifications: Vec<Notification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board_error: Option<String>,
    pub board_loading: bool,
    pub board_filter: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_error: Option<String>,
}

impl Snapshot {
    pub(crate) fn of(state: &EngineState) -> Snapshot {
        Snapshot {
            sessions: sessions_event(state),
            task_sessions: state.task_sessions.clone(),
            archived_tasks: state.archived_tasks.iter().copied().collect(),
            done_tasks: state.done_tasks.iter().copied().collect(),
            blocked_tasks: state.blocked_tasks.clone(),
            notifications: state
                .notifications
                .iter()
                .take(NOTIFICATION_LIMIT)
                .cloned()
                .collect(),
            board: state.board.clone(),
            board_error: state.board_error.clone(),
            board_loading: state.board_loading,
            board_filter: state.board_filter.as_str().to_string(),
            usage: state.usage.clone(),
            deploy: state.deploy.clone(),
            deploy_error: state.deploy_error.clone(),
        }
    }
}

/// The sessions half of a snapshot, wired for the boundary.
pub(crate) fn sessions_event(state: &EngineState) -> SessionsEvent {
    SessionsEvent {
        by_project: state
            .sessions
            .by_project()
            .iter()
            .map(|(name, sessions)| (name.clone(), sessions.iter().map(wire_session).collect()))
            .collect(),
        stats: SessionStats {
            total_sessions: state.sessions.len(),
            total_projects: state.sessions.project_count(),
        },
        discovered_dirs: state.discovered_dirs.clone(),
    }
}

/// Everything the engine announces. The `board*` and `deploy*` variants are
/// declared here because the event NAMES are the client contract; Phases 9b and
/// 10 are what actually raise them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum EngineEvent {
    Sessions(Box<SessionsEvent>),
    Notification(Box<Notification>),
    NotificationsChanged {
        ids: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<NotificationStatus>,
        #[serde(default)]
        removed: bool,
    },
    TaskDone {
        task_id: i64,
        name: String,
        project: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mr_url: Option<String>,
    },
    TaskBlocked {
        task_id: i64,
        questions: Vec<String>,
        project: String,
    },
    TaskLinked {
        task_id: i64,
        info: TaskLink,
    },
    TaskArchived {
        task_id: i64,
        session_id: String,
    },
    SessionLinked(Box<Session>),
    Usage(Option<serde_json::Value>),
    /// Phase 9b.
    Board {
        loading: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Phase 10.
    Deploy {
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    DeployRun {
        project: String,
    },
    DeployOutput {
        project: String,
        line: String,
    },
}

impl EngineEvent {
    /// The wire name, matching the Node `EVENTS` map so a client written
    /// against either implementation reads the same stream.
    pub fn name(&self) -> &'static str {
        match self {
            EngineEvent::Sessions(_) => "sessions",
            EngineEvent::Notification(_) => "notification",
            EngineEvent::NotificationsChanged { .. } => "notifications-changed",
            EngineEvent::TaskDone { .. } => "task-done",
            EngineEvent::TaskBlocked { .. } => "task-blocked",
            EngineEvent::TaskLinked { .. } => "task-linked",
            EngineEvent::TaskArchived { .. } => "task-archived",
            EngineEvent::SessionLinked(_) => "session-linked",
            EngineEvent::Usage(_) => "usage",
            EngineEvent::Board { .. } => "board",
            EngineEvent::Deploy { .. } => "deploy",
            EngineEvent::DeployRun { .. } => "deploy-run",
            EngineEvent::DeployOutput { .. } => "deploy-output",
        }
    }
}

/// The subscriber list. One `Sender` per client; a send that fails means the
/// receiver was dropped, so the subscriber is forgotten.
#[derive(Debug, Default)]
pub(crate) struct EventBus {
    subscribers: Mutex<Vec<Sender<EngineEvent>>>,
}

impl EventBus {
    pub(crate) fn subscribe(&self) -> Receiver<EngineEvent> {
        let (tx, rx) = channel();
        lock(&self.subscribers).push(tx);
        rx
    }

    pub(crate) fn publish(&self, event: EngineEvent) {
        let mut subscribers = lock(&self.subscribers);
        subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        lock(&self.subscribers).len()
    }
}
