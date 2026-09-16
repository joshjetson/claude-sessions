//! What a client can ask the engine to do.
//!
//! Every one of these is a method Phase 6's HTTP routes forward to and Phase
//! 7's TUI calls directly, so they are written to be safe from either: they
//! take owned values, they never block on a lock they also publish under, and
//! the slow ones hand their work to a thread rather than to the caller.

use std::sync::PoisonError;

use crate::scan::ProcessSource;
use crate::types::{Board, NotificationStatus};

use super::engine::Engine;
use super::events::EngineEvent;
use super::markers::{BlockedMarker, DoneMarker};
use super::notify::{ActionResult, NewNotification};
use super::pending::PendingRequest;
use super::state::{BoardFilter, TaskLinkPatch};
use super::RefreshRequest;

impl<S: ProcessSource> Engine<S> {
    /// Rebuild the sessions list now. Concurrent calls collapse into one
    /// trailing re-run — see the `refresh` module.
    pub fn refresh(&self, request: RefreshRequest) {
        self.inner.refresh(request.force_discovery);
    }

    /// Re-read the config file and rediscover, for the dashboard's `r`.
    pub fn reload_config(&self) {
        self.inner
            .config
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .reload();
        self.inner.refresh(true);
    }

    /// Which tasks the board shows. Changing it drops the board on the floor:
    /// the old one is another filter's answer, and leaving it up while the new
    /// one loads shows exactly the tasks that were just filtered out.
    pub fn set_board_filter(&self, filter: BoardFilter) -> ActionResult {
        {
            let mut state = self.inner.state();
            state.board_filter = filter;
            state.set_board(None);
        }
        ActionResult::ok()
    }

    /// Hand the engine a freshly fetched board.
    ///
    /// Phase 9b owns the fetching; what belongs here is the task index that
    /// makes `find_task_by_id` a map hit, and the event that tells clients.
    /// Passing `loading: true` with no board announces that a fetch has
    /// started, which is what keeps the board tab from looking empty.
    pub fn set_board(&self, board: Option<Board>, error: Option<String>) {
        let loading = {
            let mut state = self.inner.state();
            if board.is_some() || error.is_some() {
                state.board_loading = false;
                state.board_error = error.clone();
                if board.is_some() {
                    state.set_board(board);
                }
            } else {
                state.board_loading = true;
            }
            state.board_loading
        };
        self.inner.publish(EngineEvent::Board { loading, error });
    }

    /// The same for the deploy tab, whose shape Phase 10 defines.
    pub fn set_deploy(&self, deploy: Option<serde_json::Value>, error: Option<String>) {
        {
            let mut state = self.inner.state();
            if deploy.is_some() {
                state.deploy = deploy;
            }
            state.deploy_error = error.clone();
        }
        self.inner.publish(EngineEvent::Deploy { error });
    }

    /// A session was just launched — watch for it so it can be linked to the
    /// task that spawned it.
    pub fn set_pending(&self, request: PendingRequest) {
        self.inner.set_pending(request);
    }

    /// Record (or update) the session working a task.
    pub fn link_task(&self, task_id: i64, patch: TaskLinkPatch) {
        self.inner.link_task(task_id, patch);
    }

    pub fn push_notification(&self, notification: NewNotification) -> crate::types::Notification {
        self.inner.push_notification(notification)
    }

    pub fn set_notification_status(
        &self,
        ids: &[String],
        status: NotificationStatus,
    ) -> ActionResult {
        self.inner.set_notification_status(ids, status)
    }

    pub fn dismiss_notifications(&self, ids: &[String]) -> ActionResult {
        self.inner.dismiss_notifications(ids)
    }

    pub fn mark_notifications_read(&self, ids: &[String]) -> ActionResult {
        self.inner.mark_notifications_read(ids)
    }

    /// Ask Claude Code how much of the plan is used. Failures keep the previous
    /// numbers: a stale readout with its timestamp beats one that blinks out.
    pub fn refresh_usage(&self) -> Option<serde_json::Value> {
        let value = self.inner.usage.as_ref().map(|hook| hook())?;
        self.inner.state().usage = Some(value.clone());
        self.inner.publish(EngineEvent::Usage(Some(value.clone())));
        Some(value)
    }
}
impl<S: ProcessSource + Send + 'static> Engine<S> {
    /// An agent signed a task off — the HTTP body form of a `done/` marker.
    ///
    /// Fire and forget: the route answers 202 and the work (Odoo, `glab`, the
    /// archive) happens on a worker thread, because it takes seconds and the
    /// sessions list must keep updating while it does.
    pub fn process_done(&self, marker: DoneMarker) {
        self.inner
            .spawn_worker(move |inner| inner.process_done(marker));
    }

    /// The same for the readiness gate.
    pub fn process_blocked(&self, marker: BlockedMarker) {
        self.inner
            .spawn_worker(move |inner| inner.process_blocked(marker));
    }

    /// Fetch the board, off the caller's thread.
    ///
    /// Fire-and-forget like `processDone`: an Odoo round trip takes seconds and
    /// an HTTP route (or a keystroke) must not wait for it. Best-effort by
    /// design — a failure leaves the previous board up with an error beside it
    /// and never stops the sessions tick.
    pub fn refresh_board(&self) {
        if self.inner.fetch_board.is_none() {
            return;
        }
        self.inner.state().board_loading = true;
        self.inner.publish(EngineEvent::Board {
            loading: true,
            error: None,
        });
        self.inner.spawn_worker(|inner| inner.poll_board());
    }

    /// Wait for the completion workers. `stop` does this too; it is public so a
    /// caller that has just posted a completion can wait for it.
    pub fn join_workers(&self) {
        self.inner.join_workers();
    }
}
