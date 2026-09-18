//! The board tab: the notification feed, the Odoo task tree, and everything
//! pressing a key on one of them does.
//!
//! Ports the board halves of the Node app's `src/tui/App.js`, `controller.js`
//! and `actions.js`. Split by responsibility rather than by the Node file it
//! came from, because `actions.js` alone was 756 lines of launch flow, terminal
//! driving, stage moves, ssh and sounds:
//!
//! - [`slice`] — the state the tab draws from, and how a fetch folds into it.
//! - [`controller`] — which live session belongs to a task (written ONCE; Node
//!   had three copies).
//! - [`view`] — the row list and the cursor.
//! - [`detail`] — the right-hand pane.
//! - [`launch`] / [`spec`] — what starting a task *means*, as pure functions.
//! - [`start`] — the one entry point every launch route goes through.
//! - [`keys`] — the key map, which composes the modules above.
//!
//! Nothing in this module spawns a process or opens a socket. The launch flow
//! ends at a [`LaunchSpec`] and the worker thread in [`crate::ui::actions`]
//! executes it, which is why the two incident-driven guards (dependency gate,
//! duplicate-start guard) can be tested with no machine that could run an agent
//! against real task data.

pub mod controller;
pub mod detail;
pub mod keys;
pub mod launch;
pub mod runs;
pub mod slice;
pub mod spec;
pub mod start;
pub mod view;

pub use controller::{
    focus_task_terminal, go_to_task_session, match_session_by_cwd, resolve_notif_session,
    task_session, task_sessions, FocusTarget, SessionTarget,
};
pub use detail::TaskState;
pub use keys::handle_board;
pub use launch::{
    all_discovered_dirs, gate_start, guess_dir_for_project, prompt_context, resolve_task_dir,
    working_stage_move, DirChoice, Gate, LaunchKind, RacingSession,
};
pub use runs::{jump_to_next_ask, run_command, watch_or_drop};
pub use slice::{live_task_ids, BoardDetail, BoardSlice, BoardUpdate, DetailAnswers};
pub use spec::{
    short, LaunchSpec, NudgeSpec, PromptContext, ResumePurpose, ResumeRequest, SendSpec,
};
pub use start::{start, task_url, StartRequest};
pub use view::{label, snapshot, window, BoardRow, BoardSnapshot, BoardWindow};

use crate::daemon::PendingRequest;
use crate::odoo::FetchBoardOptions;
use crate::ui::actions::ActionResult;
use crate::ui::dialogs::Dialog;
use crate::ui::feed::SessionFeed;
use crate::ui::state::{Action, AppState};

/// Tell the feed a launch is in flight, so whatever owns the pending queue can
/// pair the new session to its task.
///
/// The link has to be registered BEFORE the terminal opens: a `claude` process
/// writes no transcript for its first seconds, and a launch that is not queued
/// by then is a session nothing will ever claim.
pub fn note_launch(feed: &dyn SessionFeed, action: &Action) {
    let Action::Launch(spec) = action else {
        return;
    };
    feed.note_task_launch(PendingRequest {
        cwd: spec.cwd.clone(),
        task_id: Some(spec.task_id),
        known_session_ids: spec.known_session_ids.clone(),
    });
}

/// Fold a worker result that belongs to the board into the state.
pub fn apply_result(state: &mut AppState, result: ActionResult) {
    match result {
        ActionResult::Board(update) => state.apply_board(*update),
        ActionResult::Data(data) => {
            // Data is addressed to whichever dialog asked for it; a stale answer
            // (the dialog was closed, or moved on to another task) is dropped
            // rather than shown against the wrong row.
            if let Some(dialog) = state.dialog.as_mut() {
                if dialog.accept(&data) {
                    state.dirty = true;
                    return;
                }
            }
            match *data {
                crate::ui::actions::BoardData::TaskDescription { task_id, detail } => {
                    detail::apply_description(state, task_id, detail.as_ref());
                }
                crate::ui::actions::BoardData::TaskOptics { task_id, optics } => {
                    detail::apply_optics(state, task_id, optics);
                }
                _ => {}
            }
        }
        ActionResult::Deploy(update) => state.apply_deploy(*update),
        // The ones the loop owns; `apply_result` is the fallback arm.
        ActionResult::Flash(_)
        | ActionResult::Refresh
        | ActionResult::Launched
        | ActionResult::Usage(_) => {}
    }
}

/// The board query for the current filter and config, built where both live.
pub fn fetch_options(state: &AppState) -> FetchBoardOptions {
    FetchBoardOptions::for_config(
        &state.config,
        state.board.filter == crate::daemon::BoardFilter::Mine,
    )
}

/// Open a dialog, replacing whatever was there.
pub(crate) fn open(state: &mut AppState, dialog: Dialog) {
    state.dialog = Some(dialog);
    state.dirty = true;
}
