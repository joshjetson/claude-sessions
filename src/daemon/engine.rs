//! The hub: what the engine owns, how it starts and stops, and the actions a
//! client can ask of it.
//!
//! Everything here is either a field, a lifecycle step or a one-line forward to
//! the module that does the work. That is the point — the Node original grew to
//! 1196 lines by keeping the scanning, the board, the deploy supervisor and the
//! HTTP glue in the same class.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard};
use std::thread::{self, JoinHandle};

use crate::archive::Archive;
use crate::config::ConfigHandle;
use crate::db::Db;
use crate::paths::Paths;
use crate::scan::{ProcessSource, Scanner, SystemProcessSource};
use crate::term::SpawnPolicy;
use crate::types::Task;

use super::caches::Caches;
use super::completion::{DailyLogHook, NullBackend, TaskBackend};
use super::events::{EngineEvent, EventBus, Snapshot};
use super::lock;
use super::state::EngineState;

/// The Odoo lookup behind the new-assignment alert.
///
/// Injected rather than called directly so the alert path is testable without
/// Odoo — the Node app did the same, for the same reason. Phase 9b supplies the
/// real one.
pub type AssignedFetch = Box<dyn Fn(&[String]) -> Result<Vec<Task>, String> + Send + Sync>;

/// The plan-usage readout. Phase 11 supplies it; the engine only carries the
/// value to clients.
pub type UsageHook = Box<dyn Fn() -> serde_json::Value + Send + Sync>;

/// What a client asked a refresh to cover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshRequest {
    pub force_discovery: bool,
    /// Phase 9b reads this; the sessions tick ignores it.
    pub board: bool,
    /// Phase 10 reads this.
    pub deploy: bool,
}

impl RefreshRequest {
    pub fn discovery() -> Self {
        RefreshRequest {
            force_discovery: true,
            ..Default::default()
        }
    }
}

/// Counters a `/health` route and the re-entrancy tests read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Ticks that actually ran.
    pub refreshes: u64,
    /// Calls that arrived while one was in flight and were folded into the
    /// single trailing re-run.
    pub collapsed: u64,
    /// Ticks that panicked and were caught. Anything above zero is a bug
    /// report; the loop kept going regardless.
    pub panics: u64,
    pub subscribers: usize,
    pub open_cursors: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) refreshes: AtomicU64,
    pub(crate) collapsed: AtomicU64,
    pub(crate) panics: AtomicU64,
}

/// The re-entrancy gate. Concurrent calls collapse into ONE trailing re-run —
/// the same three flags the Node original used, for the same reason: a client
/// mashing refresh, a launch and the timer can all land inside one tick, and
/// running the scan three times over would just make the next tick later.
#[derive(Debug, Default)]
pub(crate) struct RefreshGate {
    pub(crate) in_flight: bool,
    pub(crate) queued: bool,
    pub(crate) force: bool,
}

/// The scanner and its caches, behind one lock: a tick needs both, and holding
/// them together is what lets the task-reference cache be shared between the
/// pairing rules and the archive.
pub(crate) struct ScanState<S: ProcessSource> {
    pub(crate) scanner: Scanner<S>,
    pub(crate) caches: Caches,
}

/// Everything the engine owns. Shared as an `Arc` so the loop thread and the
/// marker workers see the same state the API methods do.
pub(crate) struct EngineInner<S: ProcessSource> {
    pub(crate) paths: Paths,
    pub(crate) db: Db,
    pub(crate) config: RwLock<ConfigHandle>,
    pub(crate) scan: Mutex<ScanState<S>>,
    pub(crate) state: Mutex<EngineState>,
    pub(crate) gate: Mutex<RefreshGate>,
    pub(crate) bus: EventBus,
    pub(crate) backend: Arc<dyn TaskBackend>,
    pub(crate) fetch_assigned: Option<AssignedFetch>,
    pub(crate) daily_log: Option<DailyLogHook>,
    pub(crate) usage: Option<UsageHook>,
    pub(crate) spawn: SpawnPolicy,
    pub(crate) shutdown: super::lifecycle::Shutdown,
    pub(crate) counters: Counters,
    pub(crate) seq: AtomicU64,
    pub(crate) workers: Mutex<Vec<JoinHandle<()>>>,
}

impl<S: ProcessSource> EngineInner<S> {
    pub(crate) fn state(&self) -> MutexGuard<'_, EngineState> {
        lock(&self.state)
    }

    pub(crate) fn scan(&self) -> MutexGuard<'_, ScanState<S>> {
        lock(&self.scan)
    }

    pub(crate) fn config(&self) -> RwLockReadGuard<'_, ConfigHandle> {
        self.config.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn archive(&self) -> Archive<'_> {
        Archive::new(&self.paths, &self.db)
    }

    pub(crate) fn publish(&self, event: EngineEvent) {
        self.bus.publish(event);
    }
}

impl<S: ProcessSource> EngineInner<S> {
    /// Wait for every marker worker to finish.
    pub(crate) fn join_workers(&self) {
        // Drained first: a worker that spawns nothing new cannot be added back,
        // and holding the lock across a join would stop one finishing.
        let workers: Vec<_> = lock(&self.workers).drain(..).collect();
        for worker in workers {
            let _ = worker.join();
        }
    }
}

impl<S: ProcessSource + Send + 'static> EngineInner<S> {
    /// Run something slow off the tick thread — an Odoo round trip, a `glab`
    /// call. A completion that took four seconds would otherwise freeze the
    /// sessions list for four seconds.
    pub(crate) fn spawn_worker(
        self: &Arc<Self>,
        work: impl FnOnce(&EngineInner<S>) + Send + 'static,
    ) {
        let inner = Arc::clone(self);
        let handle = thread::spawn(move || work(&inner));
        let mut workers = lock(&self.workers);
        // Reap anything that has already finished, so the list stays the size
        // of what is actually running.
        workers.retain(|worker| !worker.is_finished());
        workers.push(handle);
    }
}

/// How to build an engine. Fields are public and the constructors fill in the
/// defaults, so a caller overrides exactly what it cares about:
///
/// ```ignore
/// Engine::new(EngineOptions { backend, ..EngineOptions::system(paths, config) })
/// ```
pub struct EngineOptions<S: ProcessSource = SystemProcessSource> {
    pub paths: Paths,
    pub config: ConfigHandle,
    pub scanner: Scanner<S>,
    /// Phases 9b and 10 replace this with the real Odoo/GitLab implementation.
    pub backend: Arc<dyn TaskBackend>,
    pub fetch_assigned: Option<AssignedFetch>,
    /// Phase 11 wires the standup log in here.
    pub daily_log: Option<DailyLogHook>,
    pub usage: Option<UsageHook>,
    pub spawn: SpawnPolicy,
}

impl EngineOptions<SystemProcessSource> {
    /// The daemon's own configuration: a real `ps`/`lsof` scanner.
    pub fn system(paths: Paths, config: ConfigHandle) -> Self {
        let scanner = Scanner::system(paths.clone());
        EngineOptions::with_scanner(paths, config, scanner)
    }
}

impl<S: ProcessSource> EngineOptions<S> {
    pub fn with_scanner(paths: Paths, config: ConfigHandle, scanner: Scanner<S>) -> Self {
        EngineOptions {
            paths,
            config,
            scanner,
            backend: Arc::new(NullBackend),
            fetch_assigned: None,
            daily_log: None,
            usage: None,
            spawn: SpawnPolicy::detect(),
        }
    }
}

/// The engine. Build one, [`start`](Engine::start) it, subscribe to its events.
pub struct Engine<S: ProcessSource = SystemProcessSource> {
    // Visible to `lifecycle`, which owns starting and stopping.
    pub(super) inner: Arc<EngineInner<S>>,
    pub(super) loops: Mutex<Vec<JoinHandle<()>>>,
    pub(super) started: AtomicBool,
}

impl<S: ProcessSource> Engine<S> {
    pub fn new(options: EngineOptions<S>) -> Engine<S> {
        let db = Db::open(&options.paths);
        Engine {
            inner: Arc::new(EngineInner {
                paths: options.paths,
                db,
                config: RwLock::new(options.config),
                scan: Mutex::new(ScanState {
                    scanner: options.scanner,
                    caches: Caches::default(),
                }),
                state: Mutex::new(EngineState::default()),
                gate: Mutex::new(RefreshGate::default()),
                bus: EventBus::default(),
                backend: options.backend,
                fetch_assigned: options.fetch_assigned,
                daily_log: options.daily_log,
                usage: options.usage,
                spawn: options.spawn,
                shutdown: super::lifecycle::Shutdown::default(),
                counters: Counters::default(),
                seq: AtomicU64::new(0),
                workers: Mutex::new(Vec::new()),
            }),
            loops: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        }
    }

    // --- what clients read ---------------------------------------------------

    /// Everything a client that just connected needs to be current.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::of(&self.inner.state())
    }

    /// A live feed of everything the engine announces. Dropping the receiver
    /// unsubscribes.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<EngineEvent> {
        self.inner.bus.subscribe()
    }

    pub fn stats(&self) -> EngineStats {
        EngineStats {
            refreshes: self.inner.counters.refreshes.load(Ordering::Relaxed),
            collapsed: self.inner.counters.collapsed.load(Ordering::Relaxed),
            panics: self.inner.counters.panics.load(Ordering::Relaxed),
            subscribers: self.inner.bus.subscriber_count(),
            open_cursors: self.inner.scan().caches.open_cursors(),
        }
    }

    /// The store, for a caller that wants to read the archive or the log
    /// without opening a second connection.
    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    /// Whether this process may start child processes. Phases 9b and 10 gate
    /// their launches and deploys on it; the engine itself starts nothing.
    pub fn spawn_policy(&self) -> SpawnPolicy {
        self.inner.spawn
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Arc<EngineInner<S>> {
        &self.inner
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }
}
