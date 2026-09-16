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

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::daemon::client::{self, DaemonClient, SseEvent, SseMessage, Subscription};
use crate::daemon::{PendingRequest, RefreshRequest, SessionsEvent, Snapshot};
use crate::types::Notification;
use crate::ui::feed::{FeedEvent, SessionFeed};

/// A dashboard's view of a running daemon.
pub struct RemoteFeed {
    client: DaemonClient,
    events: Receiver<FeedEvent>,
    /// Held so the stream stays open; dropping it closes the subscription.
    _subscription: Subscription,
}

impl RemoteFeed {
    pub fn connect(port: u16) -> Self {
        let (sender, events) = mpsc::channel();
        let subscription = client::subscribe(port, move |message| {
            if let SseMessage::Event(event) = message {
                forward(&sender, &event);
            }
        });
        RemoteFeed {
            client: DaemonClient::new(port),
            events,
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
}

/// One SSE event as whatever the dashboard understands, or nothing.
fn forward(sender: &Sender<FeedEvent>, event: &SseEvent) {
    let feed_event = match event.event.as_str() {
        "snapshot" => serde_json::from_str::<Snapshot>(&event.data)
            .ok()
            .map(|snapshot| sessions(snapshot.sessions)),
        "sessions" => serde_json::from_str::<SessionsEvent>(&event.data)
            .ok()
            .map(sessions),
        "notification" => serde_json::from_str::<Notification>(&event.data)
            .ok()
            .map(|notification| FeedEvent::Notification(Box::new(notification))),
        // board, deploy, task-done and the link events belong to phases that
        // have not landed; an unknown event is never an error.
        _ => None,
    };
    if let Some(feed_event) = feed_event {
        let _ = sender.send(feed_event);
    }
}

fn sessions(event: SessionsEvent) -> FeedEvent {
    FeedEvent::Sessions {
        by_project: event.by_project,
        discovered: event.discovered_dirs,
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

    fn note_launch(&self) {
        // Two halves of the same thing: the daemon needs to know a launch is
        // outstanding so it can link the session that appears, and it should
        // look for it now rather than at the next tick.
        self.act(|client| {
            client.set_pending(&PendingRequest::default());
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
}
