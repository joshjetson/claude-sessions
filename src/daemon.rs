//! The headless engine: everything the dashboard does that is not drawing.
//!
//! Session scanning, the task/transcript links, done and blocked markers,
//! archiving, notifications and the alert watchers live here. It renders
//! nothing and knows nothing about a terminal, so it runs inside the TUI
//! process or alone inside the daemon and behaves identically either way.
//!
//! # The split
//!
//! The Node original was one 1196-line `engine.js` holding all of it plus the
//! Odoo board, the deploy supervisor and the HTTP wiring. Here each
//! responsibility is its own module and [`Engine`] is a thin hub that owns the
//! pieces and the timers:
//!
//! | module | what it owns |
//! |---|---|
//! | `actions` | what a client can ask the engine to do |
//! | `state` | the engine-owned state and its per-refresh indexes |
//! | `caches` | the per-tick caches: one transcript cursor per live session, compacting, task-ref retries, project discovery |
//! | `refresh` | one tick — scan, enrich, prune, group, run the watchers, emit |
//! | `pending` | the launch queue and the three rules for claiming a session |
//! | `vanished` | archiving the transcript of a session that went away |
//! | `watchers` | awaiting-decision, stall and new-assignment detection |
//! | `alerts` | the pure detection rules the watchers run |
//! | `lifecycle` | start, stop, and the one timer thread |
//! | `markers` | the `done/` and `blocked/` marker directories |
//! | `completion` | what happens when a task finishes, and the seams later phases fill |
//! | `summary` | an agent's sign-off text as the HTML Odoo's chatter wants |
//! | `notify` | the notification feed |
//! | `events` | what leaves the engine: [`EngineEvent`], [`Snapshot`], [`wire_session`] |
//!
//! # Locks
//!
//! Three: the scan (scanner plus caches), the state, and the config. They are
//! always taken in that order, and **the scan lock is never taken while the
//! state lock is held** — that rule is what keeps a slow `lsof` on the refresh
//! thread from deadlocking against a marker worker that wants to archive.
//! Poisoning is taken over rather than propagated, exactly as [`crate::db`]
//! does it: a panic in one tick must not disable the engine for the rest of the
//! process.

mod actions;
mod alerts;
mod caches;
mod completion;
mod engine;
mod events;
mod lifecycle;
mod markers;
mod notify;
mod pending;
mod refresh;
mod state;
mod summary;
mod vanished;
mod watchers;

pub use alerts::{
    detect_new_assignments, detect_stalls, human_duration, normalise_stage, NewAssignment, Stall,
    StallOptions,
};
pub use completion::{
    DailyLogHook, DailyLogRecord, DoneStageRequest, MergeRequestRequest, NullBackend, StageMove,
    TaskBackend, TaskDetail,
};
pub use engine::{AssignedFetch, Engine, EngineOptions, EngineStats, RefreshRequest, UsageHook};
pub use events::{wire_session, EngineEvent, SessionStats, SessionsEvent, Snapshot};
pub use markers::{BlockedMarker, DoneMarker, MARKER_SETTLE};
pub use notify::{ActionResult, NewNotification, NOTIFICATION_LIMIT};
pub use pending::PendingRequest;
pub use state::{
    BlockedTask, BoardFilter, EngineState, PendingLaunch, SessionIndex, TaskLink, TaskLinkPatch,
    TaskLinkStatus,
};
pub use summary::summary_to_html;
pub use watchers::{is_awaiting_user_decision, is_blocked_on_tool_call, BLOCKED_TOOL_DWELL};

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Take a lock, adopting a poisoned one rather than propagating the panic.
///
/// A tick that panicked has already been caught and counted (see the
/// `refresh` module); refusing to serve the next one because of it would turn one
/// bad transcript into a dead daemon.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
