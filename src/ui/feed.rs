//! Where session data comes from — the seam Phase 6 replaces.
//!
//! The Node app had two transports behind one set of handlers: `remote` (a
//! daemon owns the engine, the TUI subscribes over SSE) and `embedded` (the
//! engine runs in-process). [`SessionFeed`] is that seam. Phase 6 plugs the
//! daemon SSE client in here as a second implementation and nothing above this
//! module changes.
//!
//! Until then [`EmbeddedFeed`] runs an "embedded-lite" scan on its own thread:
//! the [`Scanner`] is held across ticks for its caches, and each live transcript
//! keeps a [`TranscriptCursor`] so a tick parses only the bytes appended since
//! the last one (brief §10 mandate #1). It is deliberately *lite* — no alerts,
//! no archiving, no task linking; Phase 5's daemon owns all of that.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::paths::Paths;
use crate::scan::{discover_projects, is_compacting, Scanner};
use crate::transcript::{Collect, TranscriptCursor};
use crate::types::{Notification, RawSession, Session, SessionStatus};
use crate::ui::tree::SessionsByProject;
use crate::util::{activity_label, detect_session_status, parse_timestamp, project_name};

/// Steady-state scan cadence.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// Cadence just after a launch, so a new session appears almost at once.
pub const FAST_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How long the fast cadence lasts after a launch.
pub const FAST_POLL_WINDOW: Duration = Duration::from_secs(30);
/// Project discovery is a `readdir` per configured group; it does not need to
/// happen every second.
pub const DISCOVERY_TTL: Duration = Duration::from_secs(30);

/// What the dashboard learns between ticks.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    Sessions {
        by_project: SessionsByProject,
        discovered: BTreeMap<String, Vec<String>>,
    },
    /// Phase 5/6 produce these; nothing does yet.
    Notification(Box<Notification>),
    /// One Odoo board fetch, from whichever side made it — the daemon's `board`
    /// event, or the in-process worker.
    Board(Box<crate::ui::board::BoardUpdate>),
}

/// The contract the dashboard consumes. `Send` because the daemon client will
/// own a socket.
pub trait SessionFeed: Send {
    /// Non-blocking: everything that has arrived since the last call.
    fn drain(&mut self) -> Vec<FeedEvent>;
    /// Scan now rather than at the next tick.
    fn request_refresh(&self);
    /// A session was just launched — poll faster for a while.
    fn note_launch(&self);
    /// The same, for a launch that belongs to a task: whatever owns the pending
    /// queue registers it so the new session can be paired to its task. The
    /// default is the plain fast-poll, which is all an embedded scan can do;
    /// the daemon client overrides it with `POST /session/pending`.
    fn note_task_launch(&self, request: crate::daemon::PendingRequest) {
        let _ = request;
        self.note_launch();
    }
    /// Re-read the group list (a group was added or removed).
    fn set_groups(&self, group_paths: Vec<String>);
}

enum Command {
    Refresh,
    NoteLaunch,
    SetGroups(Vec<String>),
    Stop,
}

/// Scans this machine on a background thread.
pub struct EmbeddedFeed {
    events: Receiver<FeedEvent>,
    commands: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl EmbeddedFeed {
    pub fn start(paths: Paths, group_paths: Vec<String>) -> Self {
        let (event_tx, events) = mpsc::channel();
        let (commands, command_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("claude-sessions-scan".into())
            .spawn(move || scan_loop(paths, group_paths, event_tx, command_rx))
            .ok();
        EmbeddedFeed {
            events,
            commands,
            worker,
        }
    }
}

impl SessionFeed for EmbeddedFeed {
    fn drain(&mut self) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    fn request_refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }

    fn note_launch(&self) {
        let _ = self.commands.send(Command::NoteLaunch);
    }

    fn set_groups(&self, group_paths: Vec<String>) {
        let _ = self.commands.send(Command::SetGroups(group_paths));
    }
}

impl Drop for EmbeddedFeed {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn scan_loop(
    paths: Paths,
    mut group_paths: Vec<String>,
    events: Sender<FeedEvent>,
    commands: Receiver<Command>,
) {
    let mut scanner = Scanner::system(paths);
    let mut cursors: HashMap<PathBuf, TranscriptCursor> = HashMap::new();
    let mut discovered: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut discovered_at: Option<Instant> = None;
    let mut fast_until: Option<Instant> = None;
    let mut due = Instant::now();

    loop {
        let now = Instant::now();
        let interval = match fast_until {
            Some(until) if until > now => FAST_POLL_INTERVAL,
            _ => POLL_INTERVAL,
        };
        // Wait for the next tick, but wake immediately for a command.
        let wait = due.saturating_duration_since(now);
        match commands.recv_timeout(wait) {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Command::Refresh) => due = Instant::now(),
            Ok(Command::NoteLaunch) => {
                fast_until = Some(Instant::now() + FAST_POLL_WINDOW);
                due = Instant::now();
            }
            Ok(Command::SetGroups(paths)) => {
                group_paths = paths;
                discovered_at = None;
                due = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() < due {
            continue;
        }
        due = Instant::now() + interval;

        let stale = discovered_at.is_none_or(|at| at.elapsed() >= DISCOVERY_TTL);
        if stale {
            discovered = group_paths
                .iter()
                .map(|path| {
                    let trimmed = path.trim_end_matches('/').to_string();
                    let dirs = discover_projects(std::path::Path::new(&trimmed));
                    (trimmed, dirs)
                })
                .collect();
            discovered_at = Some(Instant::now());
        }

        let wall = SystemTime::now();
        let raw = scanner.scan_sessions(wall);
        let live: HashSet<PathBuf> = raw.iter().filter_map(|s| s.session_file.clone()).collect();
        cursors.retain(|path, _| live.contains(path));

        let sessions: Vec<Session> = raw
            .into_iter()
            .map(|session| enrich(session, &mut cursors, &mut scanner, wall))
            .collect();

        // A bad tick must never kill the loop; a dead channel means the
        // dashboard has gone, which does.
        if events
            .send(FeedEvent::Sessions {
                by_project: group_sessions(sessions),
                discovered: discovered.clone(),
            })
            .is_err()
        {
            return;
        }
    }
}

/// A scanned process plus everything its transcript says.
///
/// The cursor per file is what makes this O(bytes appended) instead of Node's
/// "re-read 512KB per active session per second".
fn enrich(
    raw: RawSession,
    cursors: &mut HashMap<PathBuf, TranscriptCursor>,
    scanner: &mut Scanner,
    now: SystemTime,
) -> Session {
    let Some(path) = raw.session_file.clone() else {
        return placeholder(raw);
    };
    let cursor = match cursors.entry(path.clone()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            match TranscriptCursor::open(&path, Collect::Session) {
                Ok(cursor) => entry.insert(cursor),
                // An unreadable transcript is a session we still know is
                // running — show it, do not drop it.
                Err(_) => return placeholder(raw),
            }
        }
    };
    let _ = cursor.poll();
    let parsed = cursor.session();

    let compacting = is_compacting(&path, now);
    let status = if compacting {
        SessionStatus::Compacting
    } else {
        detect_session_status(parsed.last_entry.as_ref(), raw.session_mtime, now)
    };
    let activity_detail = if compacting {
        "compacting".to_string()
    } else {
        activity_label(parsed.last_entry.as_ref())
    };

    Session {
        // The parsed id wins: it is what the transcript calls itself.
        session_id: if parsed.session_id.is_empty() {
            raw.session_id
        } else {
            parsed.session_id
        },
        pids: raw.pids,
        cwd: raw.cwd,
        tty: raw.tty,
        lstart: raw.lstart,
        session_file: raw.session_file,
        session_mtime: raw.session_mtime,
        session_size: raw.session_size,
        status,
        activity_detail,
        starting: raw.starting,
        git_branch: Some(parsed.git_branch).filter(|b| !b.is_empty()),
        last_timestamp: Some(parsed.last_timestamp).filter(|t| !t.is_empty()),
        last_usage: parsed.last_usage,
        last_entry: parsed.last_entry,
        cumulative_usage: Some(parsed.cumulative_usage),
        prompts: parsed.prompts,
        task_id: scanner.task_refs().get(&path),
    }
}

/// A live process with no transcript yet — the `starting-<pid>` row.
fn placeholder(raw: RawSession) -> Session {
    Session {
        session_id: raw.session_id,
        pids: raw.pids,
        cwd: raw.cwd,
        tty: raw.tty,
        lstart: raw.lstart,
        session_file: raw.session_file,
        session_mtime: raw.session_mtime,
        session_size: raw.session_size,
        status: raw.status.unwrap_or(SessionStatus::Idle),
        activity_detail: String::new(),
        starting: raw.starting,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
    }
}

/// Group by display name and sort newest-first inside each project, as the Node
/// engine did — the session you touched last is the one you want at the top.
pub fn group_sessions(sessions: Vec<Session>) -> SessionsByProject {
    let mut by_project: SessionsByProject = BTreeMap::new();
    for session in sessions {
        by_project
            .entry(project_name(&session.cwd))
            .or_default()
            .push(session);
    }
    for sessions in by_project.values_mut() {
        sessions.sort_by_key(|session| std::cmp::Reverse(sort_key(session)));
    }
    by_project
}

/// Newest activity first: the transcript's own last timestamp where it has one,
/// the file's mtime otherwise.
fn sort_key(session: &Session) -> SystemTime {
    session
        .last_timestamp
        .as_deref()
        .and_then(parse_timestamp)
        .map(SystemTime::from)
        .unwrap_or(session.session_mtime)
}

/// A feed that replays a fixed script. Everything above the seam can then be
/// tested without a machine to scan.
#[derive(Debug, Default)]
pub struct StaticFeed {
    pub queued: Vec<FeedEvent>,
    pub refreshes: std::sync::atomic::AtomicUsize,
    pub launches: std::sync::atomic::AtomicUsize,
}

impl StaticFeed {
    pub fn with(events: Vec<FeedEvent>) -> Self {
        StaticFeed {
            queued: events,
            ..StaticFeed::default()
        }
    }
}

impl SessionFeed for StaticFeed {
    fn drain(&mut self) -> Vec<FeedEvent> {
        std::mem::take(&mut self.queued)
    }

    fn request_refresh(&self) {
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn note_launch(&self) {
        self.launches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn set_groups(&self, _group_paths: Vec<String>) {}
}
