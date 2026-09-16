//! Starting, stopping, and the one timer thread in between.
//!
//! The Node original ran four `setInterval`s and a `setTimeout` per launch, and
//! every one of them had to be `unref`ed so a finished process was not held
//! open by a pending timer. Here there is one thread with deadlines: the
//! intervals cannot drift apart, a slow tick cannot overlap itself, and
//! [`Engine::stop`] has exactly one thing to join.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::scan::ProcessSource;

use super::engine::{Engine, EngineInner};
use super::events::EngineEvent;
use super::lock;

/// How often the sessions list is rebuilt.
const TICK: Duration = Duration::from_secs(1);
/// …and how often while a launch is still waiting for its session, so a new
/// session shows up within half a second of being spawned.
const FAST_TICK: Duration = Duration::from_millis(500);
/// The slow cadence: the board poll and the new-assignment watcher.
const SLOW_TICK: Duration = Duration::from_secs(45);

/// A stop flag with a condition variable, so a sleeping loop wakes the instant
/// [`Engine::stop`] is called instead of at the end of its interval.
///
/// The Node equivalent was `timer.unref()`: a pending timer must never be the
/// reason a finished process stays alive.
#[derive(Debug, Default)]
pub(crate) struct Shutdown {
    stopping: Mutex<bool>,
    wake: Condvar,
}

impl Shutdown {
    fn stop(&self) {
        *lock(&self.stopping) = true;
        self.wake.notify_all();
    }

    fn reset(&self) {
        *lock(&self.stopping) = false;
    }

    /// Sleep, or return early. `false` means "stop now".
    fn sleep(&self, duration: Duration) -> bool {
        let guard = lock(&self.stopping);
        if *guard {
            return false;
        }
        let (guard, _) = self
            .wake
            .wait_timeout(guard, duration)
            .unwrap_or_else(PoisonError::into_inner);
        !*guard
    }
}

impl<S: ProcessSource + Send + 'static> Engine<S> {
    /// Load, seed, clean, refresh once, then start the loop.
    ///
    /// The first refresh is synchronous on purpose: by the time this returns, a
    /// client asking for a snapshot gets real sessions rather than an empty
    /// tree it has to watch fill in.
    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.shutdown.reset();

        self.inner
            .config
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .reload();
        // Fold any pre-database files into SQLite. Idempotent, so it is safe on
        // every start; it only fills gaps.
        self.inner.db.import_existing_files(&self.inner.paths);
        {
            let archived = self.inner.archive().list_archived_task_ids();
            let mut state = self.inner.state();
            state.archived_tasks.extend(archived);
        }
        self.inner.restore_notifications();
        // Clear markers left by a previous run BEFORE the first poll, so an old
        // completion is never replayed as a new one.
        self.inner.clean_stale_markers();

        self.inner.refresh(true);
        self.inner.notify_new_assignments();

        let usage_interval = self.inner.config().usage().interval;
        if self.inner.usage.is_some() {
            self.refresh_usage();
        }

        let inner = Arc::clone(&self.inner);
        let handle = thread::Builder::new()
            .name("claude-sessions-engine".to_string())
            .spawn(move || run_loop(&inner, usage_interval))
            .expect("engine loop thread");
        lock(&self.loops).push(handle);
    }

    /// Stop the loop and wait for everything in flight.
    pub fn stop(&self) {
        if !self.started.swap(false, Ordering::SeqCst) {
            return;
        }
        self.inner.shutdown.stop();
        for handle in lock(&self.loops).drain(..) {
            let _ = handle.join();
        }
        self.inner.join_workers();
    }
}

/// The one timer thread: sessions at 1s (500ms while a launch is in flight),
/// markers on the same beat, the assignment watcher every 45s, and usage on its
/// configured interval.
///
/// One thread with deadlines rather than Node's four `setInterval`s: they
/// cannot drift apart, a slow tick cannot overlap itself, and there is exactly
/// one thing for [`Engine::stop`] to join.
fn run_loop<S: ProcessSource + Send + 'static>(
    inner: &Arc<EngineInner<S>>,
    usage_interval: Option<Duration>,
) {
    let mut next_slow = Instant::now() + SLOW_TICK;
    let mut next_usage = usage_interval.map(|interval| Instant::now() + interval);

    while inner.shutdown.sleep(inner.tick_interval()) {
        inner.refresh(false);
        inner.poll_markers();

        let now = Instant::now();
        if now >= next_slow {
            next_slow = now + SLOW_TICK;
            inner.notify_new_assignments();
        }
        if let (Some(at), Some(interval)) = (next_usage, usage_interval) {
            if now >= at {
                next_usage = Some(now + interval);
                if let Some(hook) = &inner.usage {
                    let value = hook();
                    inner.state().usage = Some(value.clone());
                    inner.publish(EngineEvent::Usage(Some(value)));
                }
            }
        }
    }
}

impl<S: ProcessSource> EngineInner<S> {
    /// Twice as fast while a launch is still waiting for its session — the
    /// window is the launch queue's own, so it ends when the last launch
    /// resolves or expires rather than on a separate timer.
    fn tick_interval(&self) -> Duration {
        if self.state().has_pending() {
            FAST_TICK
        } else {
            TICK
        }
    }

    /// Now, as one instant for a whole tick to judge against.
    pub(crate) fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    /// The runtime directories the marker watchers own.
    pub(crate) fn marker_dirs(&self) -> [PathBuf; 2] {
        [self.paths.done_dir.clone(), self.paths.blocked_dir.clone()]
    }
}

impl<S: ProcessSource> Drop for Engine<S> {
    fn drop(&mut self) {
        self.inner.shutdown.stop();
        for handle in lock(&self.loops).drain(..) {
            let _ = handle.join();
        }
    }
}
