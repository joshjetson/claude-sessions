//! The Deploy tab: configured projects, their unfinished Deployed-stage tasks
//! with live merge-request badges, and the output of the deploy command.
//!
//! Ports the deploy halves of the Node app's `src/tui/App.js` and the whole of
//! `src/tui/deployactions.js`. Split the same way the board tab is:
//!
//! - [`slice`] — the state the tab draws from, and how an update folds in.
//! - [`view`] — the row list and the cursor.
//! - [`detail`] — the right-hand pane: a task, a project, or a run's output.
//! - [`keys`] — the key map.
//!
//! Nothing here merges, deploys or talks to GitLab. Every one of those leaves
//! as an [`Action`](crate::ui::state::Action) and the worker in
//! [`crate::ui::actions`] runs it — which is what lets the whole tab, dialogs
//! included, be driven in a test with no `glab` on the machine and no
//! possibility of firing a production deploy.
//!
//! One rule the tab is built around: **the deploy board refreshes only when
//! asked**. Every refresh costs one GitLab API call per open merge request, so
//! no timer touches it and switching to the tab does not either.

pub mod detail;
pub mod keys;
pub mod slice;
pub mod view;

pub use detail::{redraw, refresh_detail, show_project, show_row, show_run};
pub use keys::{blocked_count, handle_deploy, merge_target, ready_targets};
pub use slice::{DeploySlice, DeployUpdate};
pub use view::{build_items, label, snapshot, window, DeployRow, DeploySnapshot, DeployWindow};

use crate::ui::state::AppState;

/// Fold a deploy-board update into the state, from whichever side produced it.
pub fn apply_result(state: &mut AppState, update: DeployUpdate) {
    state.deploy.apply(update);
    // Whatever pane is open was rendered from the board that just changed.
    refresh_detail(state);
    state.dirty = true;
}
