//! Who is listening, and when to stop.
//!
//! The two pieces of shared state the server has: the stop flag its threads
//! watch, and the set of connected SSE clients. Kept apart from the transport
//! because the hardening rule that matters most — a client too far behind is
//! dropped, not buffered — lives here and should be readable on its own.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::daemon::lock;

/// A client whose unsent backlog passes this is dropped rather than buffered.
pub const MAX_CLIENT_BACKLOG: usize = 4_000_000;

/// Set by `/shutdown` or by [`ServerHandle::stop`]; waited on by the daemon's
/// main thread.
#[derive(Debug, Default)]
pub(super) struct Stop {
    stopping: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    pub(super) fn request(&self) {
        *lock(&self.stopping) = true;
        self.wake.notify_all();
    }

    pub(super) fn is_stopping(&self) -> bool {
        *lock(&self.stopping)
    }

    /// Wait for the stop, or for `timeout`. True once stopping.
    pub(super) fn wait_timeout(&self, timeout: Duration) -> bool {
        let guard = lock(&self.stopping);
        if *guard {
            return true;
        }
        let (guard, _) = self
            .wake
            .wait_timeout(guard, timeout)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

struct SseClient {
    id: u64,
    frames: Sender<Arc<String>>,
    /// Bytes queued for this client but not yet written.
    pending: Arc<AtomicUsize>,
}

/// Every connected SSE client. The only shared mutable state the server has.
#[derive(Default)]
pub(super) struct Clients {
    next_id: AtomicU64,
    list: Mutex<Vec<SseClient>>,
}

/// Would this frame put the client over its backlog allowance?
///
/// Pulled out so the rule is testable without a slow client to simulate.
pub fn over_backlog(pending: usize, frame: usize) -> bool {
    pending.saturating_add(frame) > MAX_CLIENT_BACKLOG
}

impl Clients {
    pub(super) fn add(&self) -> (u64, Receiver<Arc<String>>, Arc<AtomicUsize>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (frames, receiver) = channel();
        let pending = Arc::new(AtomicUsize::new(0));
        lock(&self.list).push(SseClient {
            id,
            frames,
            pending: Arc::clone(&pending),
        });
        (id, receiver, pending)
    }

    pub(super) fn remove(&self, id: u64) {
        lock(&self.list).retain(|client| client.id != id);
    }

    pub(super) fn len(&self) -> usize {
        lock(&self.list).len()
    }

    /// Queue one frame for every client, dropping the ones that are gone or too
    /// far behind. Dropping a client drops its sender, which is what tells its
    /// writer thread to close the socket and exit.
    pub(super) fn broadcast(&self, frame: &Arc<String>) {
        let mut list = lock(&self.list);
        if list.is_empty() {
            return;
        }
        list.retain(|client| {
            if over_backlog(client.pending.load(Ordering::Relaxed), frame.len()) {
                return false;
            }
            client.pending.fetch_add(frame.len(), Ordering::Relaxed);
            client.frames.send(Arc::clone(frame)).is_ok()
        });
    }

    pub(super) fn clear(&self) {
        lock(&self.list).clear();
    }
}
