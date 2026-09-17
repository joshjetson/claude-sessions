//! Which transport the dashboard is running on, and whether it is delivering.
//!
//! The seam [`SessionFeed`] defines says where sessions come from; this says
//! whether they are actually arriving. Three developers installed v1.0.1, each
//! had live sessions, and each got an empty pane — and an empty pane is what
//! this tool looks like when it is working perfectly on a quiet machine, so
//! nobody could tell the two apart. Everything here exists to make them
//! distinguishable: the status bar names the transport and how long ago it
//! last said anything, and a remote feed that has delivered NOTHING inside
//! [`FIRST_PAYLOAD_DEADLINE`] is replaced by an in-process scan with a
//! sentence saying so.
//!
//! The swap is deliberately one-way and deliberately simple. A daemon that has
//! not spoken in five seconds of loopback is not slow, it is wrong, and the
//! dashboard's job is to show the machine's sessions rather than to keep
//! faith with a transport.

use std::thread;
use std::time::{Duration, Instant};

use crate::paths::Paths;
use crate::ui::feed::{EmbeddedFeed, FeedEvent, SessionFeed};

/// How long a remote feed gets to deliver its first payload.
///
/// Long enough for a daemon that is mid-scan on a busy machine (a first tick
/// reads every process and every live transcript), short enough that nobody
/// sits looking at an empty list wondering.
pub const FIRST_PAYLOAD_DEADLINE: Duration = Duration::from_secs(5);

/// Where the sessions on screen come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transport {
    /// A daemon on this port owns the engine and the dashboard mirrors it.
    Daemon(u16),
    /// The dashboard scans this machine itself.
    #[default]
    Embedded,
}

impl Transport {
    pub fn label(&self) -> String {
        match self {
            Transport::Daemon(port) => format!("daemon :{port}"),
            Transport::Embedded => "embedded".to_string(),
        }
    }
}

/// What the status bar draws about the feed.
///
/// `Copy`, so the loop can refresh it every frame without allocating; the
/// label is built only when a frame is actually painted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedStatus {
    pub transport: Transport,
    /// When a sessions payload last arrived. `None` means not once yet, which
    /// is the state this whole module is about.
    pub last_payload: Option<Instant>,
}

impl FeedStatus {
    /// `daemon :41777 · 2s`, or `daemon :41777 · no data` before the first
    /// payload. Short because it shares one row with the clock and the keys.
    pub fn label(&self, now: Instant) -> String {
        let age = match self.last_payload {
            Some(at) => format!("{}s", now.saturating_duration_since(at).as_secs()),
            None => "no data".to_string(),
        };
        format!("{} · {age}", self.transport.label())
    }
}

/// The feed the loop is running on, plus the decision to stop trusting it.
pub struct FeedHandle {
    feed: Box<dyn SessionFeed>,
    status: FeedStatus,
    /// When the CURRENT transport started, which is what the deadline is
    /// measured from — a reconnect restarts the clock.
    started: Instant,
    deadline: Duration,
}

impl FeedHandle {
    /// A dashboard mirroring a daemon.
    pub fn remote(port: u16, feed: Box<dyn SessionFeed>) -> Self {
        FeedHandle::new(Transport::Daemon(port), feed)
    }

    /// A dashboard scanning for itself.
    pub fn embedded(paths: &Paths, group_paths: Vec<String>) -> Self {
        FeedHandle::new(
            Transport::Embedded,
            Box::new(EmbeddedFeed::start(paths.clone(), group_paths)),
        )
    }

    pub fn new(transport: Transport, feed: Box<dyn SessionFeed>) -> Self {
        FeedHandle {
            feed,
            status: FeedStatus {
                transport,
                last_payload: None,
            },
            started: Instant::now(),
            deadline: FIRST_PAYLOAD_DEADLINE,
        }
    }

    /// The deadline named explicitly, so a test drives the give-up path
    /// without waiting it out.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn status(&self) -> FeedStatus {
        self.status
    }

    /// Everything the loop asks of a feed that is not draining it. `&dyn`
    /// rather than the concrete type, because which one it is changes.
    pub fn feed(&self) -> &dyn SessionFeed {
        self.feed.as_ref()
    }

    /// Drain the feed, noticing whether it said anything about sessions.
    ///
    /// A payload counts even when it is empty: a daemon reporting "no sessions
    /// here" is a working daemon, and swapping away from it would be wrong.
    /// Silence is the failure, not emptiness.
    pub fn drain(&mut self) -> Vec<FeedEvent> {
        let events = self.feed.drain();
        if events
            .iter()
            .any(|event| matches!(event, FeedEvent::Sessions { .. }))
        {
            self.status.last_payload = Some(Instant::now());
        }
        events
    }

    /// Take over the scan when the remote feed has delivered nothing at all.
    ///
    /// `Some(notice)` when it swapped — one line, to be shown. Retrying the
    /// daemon in the background is deliberately not attempted: a dashboard
    /// showing this machine's sessions is the outcome, and a second transport
    /// waiting to change the answer under the user is not part of it.
    pub fn fall_back_if_silent(
        &mut self,
        paths: &Paths,
        group_paths: Vec<String>,
    ) -> Option<String> {
        let Transport::Daemon(port) = self.status.transport else {
            return None;
        };
        if self.status.last_payload.is_some() || self.started.elapsed() < self.deadline {
            return None;
        }

        let replaced = std::mem::replace(
            &mut self.feed,
            Box::new(EmbeddedFeed::start(paths.clone(), group_paths)),
        );
        // Dropping a remote feed closes its subscription, which joins a reader
        // thread that may be part-way through a reconnect backoff. That wait
        // does not belong on the draw loop.
        let _ = thread::Builder::new()
            .name("claude-sessions-feed-drop".into())
            .spawn(move || drop(replaced));

        self.status = FeedStatus {
            transport: Transport::Embedded,
            last_payload: None,
        };
        self.started = Instant::now();
        Some(format!(
            "The daemon on :{port} sent no sessions within {}s — scanning from this dashboard \
             instead. `claude-sessions doctor` says why.",
            self.deadline.as_secs()
        ))
    }
}
