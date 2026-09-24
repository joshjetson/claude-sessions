//! The remote transport: a daemon owns the engine and the dashboard mirrors it.
//!
//! This is the other half of the seam [`SessionFeed`] defines. Where
//! [`EmbeddedFeed`](super::feed::EmbeddedFeed) scans this machine itself, a
//! [`RemoteFeed`] subscribes to a daemon's SSE stream and posts actions back to
//! it — so the board polling, the alert watchers and (from Phase 10) the
//! deploys keep running after the dashboard is closed, which is the whole
//! reason the daemon exists.
//!
//! Only the events the sessions view draws are mapped. The snapshot that
//! arrives on connect becomes the same `Sessions` event a tick produces, so
//! reconnecting after the daemon restarted redraws from scratch with no special
//! case above this module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use crate::daemon::client::{self, DaemonClient, SseEvent, SseMessage, Subscription};
use crate::daemon::{BoardFilter, PendingRequest, RefreshRequest, SessionsEvent, Snapshot};
use crate::types::{DeployRun, Notification};
use crate::ui::board::BoardUpdate;
use crate::ui::deploy::DeployUpdate;
use crate::ui::feed::{FeedEvent, SessionFeed};

/// A dashboard's view of a running daemon.
pub struct RemoteFeed {
    client: DaemonClient,
    events: Receiver<FeedEvent>,
    /// The other end of `events`, so an action posted on a worker thread can
    /// report a refusal back into the pane.
    sender: Sender<FeedEvent>,
    /// Held so the stream stays open; dropping it closes the subscription.
    _subscription: Subscription,
}

impl RemoteFeed {
    pub fn connect(port: u16) -> Self {
        let (sender, events) = mpsc::channel();
        let reply = sender.clone();
        let stream_client = DaemonClient::new(port);
        let kicks = Arc::new(Kicks::default());
        let subscription = client::subscribe(port, move |message| {
            if let SseMessage::Event(event) = message {
                forward(stream_client, &kicks, &sender, &event);
            }
        });
        RemoteFeed {
            client: DaemonClient::new(port),
            events,
            sender: reply,
            _subscription: subscription,
        }
    }

    pub fn port(&self) -> u16 {
        self.client.port()
    }

    /// Post an action without blocking the draw loop. A POST waits up to ten
    /// seconds for the daemon; the dashboard has a frame to render.
    fn act(&self, action: impl FnOnce(DaemonClient) + Send + 'static) {
        let client = self.client;
        let _ = thread::Builder::new()
            .name("claude-sessions-action".into())
            .spawn(move || action(client));
    }

    /// The same, for an action whose refusal has to reach the pane. The daemon
    /// answers deploy actions with 409 and a sentence; swallowing it would
    /// leave a keypress looking like it did nothing.
    fn act_reporting(&self, action: impl FnOnce(DaemonClient) -> Option<String> + Send + 'static) {
        let client = self.client;
        let sender = self.sender.clone();
        let _ = thread::Builder::new()
            .name("claude-sessions-action".into())
            .spawn(move || {
                if let Some(message) = action(client) {
                    let _ = sender.send(FeedEvent::Flash(message));
                }
            });
    }
}

/// One SSE event as whatever the dashboard understands, or nothing.
fn forward(client: DaemonClient, kicks: &Kicks, sender: &Sender<FeedEvent>, event: &SseEvent) {
    match event.event.as_str() {
        "snapshot" => {
            if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&event.data) {
                kick(client, kicks, &snapshot);
                let board = board_update(&snapshot);
                if let Some(usage) = usage_event(snapshot.usage.clone()) {
                    let _ = sender.send(usage);
                }
                let deploy = deploy_update(&snapshot);
                // Runs first: a deploy started before this dashboard connected
                // has output to show, and an `deploy-output` line with no run
                // to attach it to is dropped.
                for run in snapshot.deploy_runs.values() {
                    let _ = sender.send(FeedEvent::DeployRun(Box::new(run.clone())));
                }
                let _ = sender.send(sessions(snapshot.sessions));
                let _ = sender.send(board);
                let _ = sender.send(deploy);
            }
        }
        "sessions" => {
            if let Ok(event) = serde_json::from_str::<SessionsEvent>(&event.data) {
                let _ = sender.send(sessions(event));
            }
        }
        "notification" => {
            if let Ok(notification) = serde_json::from_str::<Notification>(&event.data) {
                let _ = sender.send(FeedEvent::Notification(Box::new(notification)));
            }
        }
        // The board event on the wire is a light "it changed" signal
        // ({loading, error}) — the data itself rides the snapshot, so a
        // settled change is answered by refetching /state. The loading=true
        // edge is skipped: the dashboard keeps showing the last board rather
        // than blanking for the seconds an Odoo round trip takes.
        "board" => {
            let settled = serde_json::from_str::<serde_json::Value>(&event.data)
                .map(|d| d["loading"] != serde_json::Value::Bool(true))
                .unwrap_or(false);
            if settled {
                if let Some(snapshot) = client.state() {
                    let _ = sender.send(board_update(&snapshot));
                }
            }
        }
        "usage" => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.data) {
                if let Some(usage) = usage_event(Some(value)) {
                    let _ = sender.send(usage);
                }
            }
        }
        // The deploy board rides the snapshot for the same reason the board
        // does: the event is a light "it settled" signal.
        "deploy" => {
            if let Some(snapshot) = client.state() {
                let _ = sender.send(deploy_update(&snapshot));
            }
        }
        "deploy-run" => {
            if let Ok(run) = serde_json::from_str::<DeployRun>(&event.data) {
                let _ = sender.send(FeedEvent::DeployRun(Box::new(run)));
            }
        }
        "deploy-output" => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.data) {
                let project = value["project"].as_str().unwrap_or_default().to_string();
                let line = value["line"].as_str().unwrap_or_default().to_string();
                if !project.is_empty() {
                    let _ = sender.send(FeedEvent::DeployOutput { project, line });
                }
            }
        }
        // The link events belong to a phase that has not landed; an unknown
        // event is never an error.
        _ => {}
    }
}

/// The board half of a snapshot, in the shape the dashboard applies.
fn board_update(snapshot: &Snapshot) -> FeedEvent {
    FeedEvent::Board(Box::new(BoardUpdate {
        board: snapshot.board.clone(),
        error: snapshot.board_error.clone(),
        loading: snapshot.board_loading,
        filter: match snapshot.board_filter.as_str() {
            "all" => BoardFilter::All,
            _ => BoardFilter::Mine,
        },
        task_sessions: snapshot.task_sessions.clone(),
        done_tasks: snapshot.done_tasks.clone(),
        archived_tasks: snapshot.archived_tasks.clone(),
        blocked_tasks: snapshot
            .blocked_tasks
            .iter()
            .map(|(task_id, blocked)| (*task_id, blocked.questions.clone()))
            .collect(),
        // The daemon does not read Optics; the dashboard's own worker fetches
        // coverage alongside its board refresh.
        optics_tasks: std::collections::HashMap::new(),
    }))
}

/// What this dashboard has already asked its daemon for, once.
#[derive(Debug, Default)]
pub(crate) struct Kicks {
    board: AtomicBool,
    deploy: AtomicBool,
}

impl Kicks {
    /// Which of the two this snapshot shows missing and nobody has asked for
    /// yet. Claiming and reporting in one step is what keeps a reconnect from
    /// asking again: the subscription retries on a backoff, and a nudge inside
    /// that loop would turn a flapping daemon into a poll.
    pub(crate) fn claim(&self, snapshot: &Snapshot) -> (bool, bool) {
        let board = snapshot.board.is_none()
            && snapshot.board_error.is_none()
            // `loading` means a fetch is already in flight; that one announces
            // itself when it settles.
            && !snapshot.board_loading
            && !self.board.swap(true, Ordering::SeqCst);
        let deploy = snapshot.deploy.is_none()
            && snapshot.deploy_error.is_none()
            && !self.deploy.swap(true, Ordering::SeqCst);
        (board, deploy)
    }
}

/// Ask a daemon that has nothing to show for the thing it has not fetched yet.
///
/// A daemon warms its board without waiting for it — Node did the same, because
/// `start` is what a client waits on and an Odoo round trip is seconds — so a
/// dashboard that connects inside that window gets a snapshot with no board in
/// it and, until now, nothing that would ever fill it but pressing `r`.
///
/// Unconditional: the daemon no-ops when it has no Odoo credentials, which is
/// cheaper than asking it first.
fn kick(client: DaemonClient, kicks: &Kicks, snapshot: &Snapshot) {
    let (board, deploy) = kicks.claim(snapshot);
    if !board && !deploy {
        return;
    }
    // Off the reader thread: `/refresh` runs a scan before it answers, and the
    // stream has to keep draining while it does.
    let _ = thread::Builder::new()
        .name("claude-sessions-kick".into())
        .spawn(move || {
            client.refresh(RefreshRequest {
                board,
                deploy,
                ..RefreshRequest::default()
            });
        });
}

/// The daemon carries usage as opaque JSON so the wire never constrains what a
/// reading holds; the dashboard is the one that knows its shape.
fn usage_event(value: Option<serde_json::Value>) -> Option<FeedEvent> {
    let snapshot: crate::usage::UsageSnapshot = serde_json::from_value(value?).ok()?;
    Some(FeedEvent::Usage(Box::new(snapshot)))
}

/// The deploy half of a snapshot, in the shape the dashboard applies.
fn deploy_update(snapshot: &Snapshot) -> FeedEvent {
    FeedEvent::Deploy(Box::new(DeployUpdate {
        board: snapshot.deploy.clone(),
        error: snapshot.deploy_error.clone(),
        loading: false,
    }))
}

fn sessions(event: SessionsEvent) -> FeedEvent {
    FeedEvent::Sessions {
        by_project: event.by_project,
        discovered: event.discovered_dirs,
        scan_complete: event.scan_complete,
    }
}

impl SessionFeed for RemoteFeed {
    fn drain(&mut self) -> Vec<FeedEvent> {
        self.events.try_iter().collect()
    }

    fn request_refresh(&self) {
        self.act(|client| {
            client.refresh(RefreshRequest::default());
        });
    }

    /// The daemon owns the usage hook, so it takes the reading and announces
    /// it — one check, however many dashboards are attached.
    fn refresh_usage(&self) -> bool {
        self.act(|client| {
            client.refresh_usage();
        });
        true
    }

    fn note_launch(&self) {
        // Two halves of the same thing: the daemon needs to know a launch is
        // outstanding so it can link the session that appears, and it should
        // look for it now rather than at the next tick.
        self.act(|client| {
            client.set_pending(&PendingRequest::default());
            client.refresh(RefreshRequest::discovery());
        });
    }

    fn note_task_launch(&self, request: PendingRequest) {
        // The argument-carrying half of note_launch: the daemon can only link
        // the session that appears if it knows the launch's cwd and task.
        self.act(move |client| {
            client.set_pending(&request);
            client.refresh(RefreshRequest::discovery());
        });
    }

    fn set_groups(&self, _group_paths: Vec<String>) {
        // The daemon reads the group list from the same config file this
        // dashboard just wrote, so it is told to rediscover rather than told
        // what to discover.
        self.act(|client| {
            client.refresh(RefreshRequest::discovery());
        });
    }

    fn shutdown_daemon(&self) {
        self.client.shutdown();
    }

    fn start_deploy(&self, project: &str) -> Option<String> {
        let project = project.to_string();
        self.act_reporting(move |client| {
            let response = client.start_deploy(&project)?;
            if response.accepted() {
                return None;
            }
            // The daemon's own sentence: "already deploying", "no command
            // configured", or the spawn policy's refusal.
            Some(
                response.body["error"]
                    .as_str()
                    .unwrap_or("the daemon refused the deploy")
                    .to_string(),
            )
        });
        None
    }

    fn cancel_deploy(&self, project: &str) {
        let project = project.to_string();
        self.act(move |client| {
            client.cancel_deploy(&project);
        });
    }
}
