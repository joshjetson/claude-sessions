//! Auto QA, dashboard side: an arrival joins its run, and the run starts it.
//!
//! The daemon finds the arrivals (see [`crate::daemon::AutoQaFeed`]). This side
//! owns the runs, so it does the rest, with the machinery a run already has:
//!
//! 1. The task joins the run for its project and stage, the run `R` makes.
//! 2. That run gets a coordinator if none is running. A coordinator that is
//!    running is told the task joined, because its task list was fixed when it
//!    started.
//! 3. The run fills its free lanes. Admission still decides: a task Odoo has
//!    closed, one already running, and the lane limit all hold.
//! 4. The daemon is told the arrivals joined, so it hands them over once.
//!
//! Nothing here raises a notification. A session that starts is not news, and
//! it says so itself when it needs you.

use std::time::{Duration, SystemTime};

use crate::daemon::AutoQaFeed;
use crate::ui::state::{Action, AppState};

use super::runs::{fill_lanes_quiet, start_coordinator};

/// How long a coordinator launch is given to appear in a scan before Auto QA
/// may launch another for the same run.
const COORDINATOR_GRACE: Duration = Duration::from_secs(180);

/// Apply what the daemon published.
pub fn apply_auto_qa(state: &mut AppState, feed: AutoQaFeed) {
    state.board.auto_qa_tasks = feed.tasks.into_iter().map(|task| (task.id, task)).collect();
    if feed.arrivals.is_empty() {
        state.dirty = true;
        return;
    }

    let mut joined = Vec::new();
    let mut runs: Vec<String> = Vec::new();
    for arrival in feed.arrivals {
        joined.push(arrival.key);
        // The daemon reads the same config, so this only differs for the
        // second it takes to notice an edit. Confirmed either way, so a
        // project switched off does not start it later.
        if !state.config.qa_auto(&arrival.project) {
            continue;
        }
        let run_id = state
            .board
            .join_run(&arrival.project, &arrival.stage, arrival.task_id);
        tell_coordinator(state, &run_id, arrival.task_id);
        if !runs.contains(&run_id) {
            runs.push(run_id);
        }
    }
    state.save_runs();
    state.enqueue(Action::AutoQaJoined(joined));

    for run_id in runs {
        ensure_coordinator(state, &run_id);
        fill_lanes_quiet(state, &run_id);
    }
    state.dirty = true;
}

/// Start the run's coordinator unless one is running, or was launched a
/// moment ago and has not shown up in a scan yet.
fn ensure_coordinator(state: &mut AppState, run_id: &str) {
    if crate::qarun::coordinator_of(run_id, state.sessions()).is_some() {
        return;
    }
    let now = SystemTime::now();
    let recent = state
        .auto_coordinator_at
        .get(run_id)
        .is_some_and(|at| now.duration_since(*at).unwrap_or_default() < COORDINATOR_GRACE);
    if recent {
        return;
    }
    state.auto_coordinator_at.insert(run_id.to_string(), now);
    start_coordinator(state, run_id, "");
}

/// A running coordinator's task list was fixed when it started, so it is told
/// about a task that joined since. A run without one running gets one, which
/// starts with the whole list, so there is nobody to tell.
fn tell_coordinator(state: &mut AppState, run_id: &str, task_id: i64) {
    let Some(session) = crate::qarun::coordinator_of(run_id, state.sessions()) else {
        return;
    };
    let spec = crate::ui::board::NudgeSpec {
        run_id: run_id.to_string(),
        session: crate::term::SessionRef {
            tty: session.tty.clone(),
            session_id: Some(session.session_id.clone()),
            cwd: Some(session.cwd.clone()),
        },
        session_id: session.session_id.clone(),
        text: format!(
            "Task #{task_id} has joined this run through Auto QA. Add {task_id} to the \
             --tasks list whenever you run qa-status, and watch it like the others."
        ),
    };
    state.enqueue(Action::NudgeCoordinator(Box::new(spec)));
}
