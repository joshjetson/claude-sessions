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

use super::runs::{fill_lanes_quiet, refusal_for, start_coordinator};

/// How long a coordinator launch is given to appear in a scan before Auto QA
/// may launch another for the same run.
const COORDINATOR_GRACE: Duration = Duration::from_secs(180);

/// Apply what the daemon published.
pub fn apply_auto_qa(state: &mut AppState, mut feed: AutoQaFeed) {
    let tasks = std::mem::take(&mut feed.tasks);
    let arrivals = std::mem::take(&mut feed.arrivals);
    state.board.auto_qa_tasks = tasks.into_iter().map(|task| (task.id, task)).collect();
    state.board.auto_qa_status = Some(feed);
    if arrivals.is_empty() {
        state.dirty = true;
        return;
    }

    // Every key is confirmed again, so a confirmation that was lost is sent
    // once more. Only an arrival not seen before joins a run: the daemon hands
    // one over until it hears back, and a second join would nudge the
    // coordinator twice.
    let joined: Vec<String> = arrivals.iter().map(|arrival| arrival.key.clone()).collect();
    let mut placed: Vec<(String, i64)> = Vec::new();
    for arrival in arrivals {
        if !state.auto_qa_handled.insert(arrival.key.clone()) {
            continue;
        }
        // The daemon reads the same config, so this only differs for the
        // second it takes to notice an edit. Confirmed either way, so a
        // project switched off does not start it later.
        if !state.config.qa_auto(&arrival.project) {
            trace(&format!(
                "#{}: Auto QA is off for {} here, so it was not started.",
                arrival.task_id, arrival.project
            ));
            continue;
        }
        let run_id = state
            .board
            .join_run(&arrival.project, &arrival.stage, arrival.task_id);
        tell_coordinator(state, &run_id, arrival.task_id);
        placed.push((run_id, arrival.task_id));
    }
    state.save_runs();
    state.enqueue(Action::AutoQaJoined(joined));

    let mut runs: Vec<&str> = Vec::new();
    for (run_id, _) in &placed {
        if !runs.contains(&run_id.as_str()) {
            runs.push(run_id);
        }
    }
    for run_id in runs {
        ensure_coordinator(state, run_id);
        fill_lanes_quiet(state, run_id);
    }
    for (run_id, task_id) in &placed {
        let started = state
            .board
            .runs
            .iter()
            .any(|run| run.id == *run_id && run.spawned.contains(task_id));
        let outcome = if started {
            "its session started".to_string()
        } else {
            match refusal_for(state, run_id, *task_id) {
                Some(why) => format!("it waits: {why}"),
                None => "it waits for a free lane".to_string(),
            }
        };
        trace(&format!("#{task_id} joined run {run_id}, and {outcome}"));
    }
    state.dirty = true;
}

/// Ask the daemon to read the QA stages now when the board shows a task in QA
/// for an Auto QA project that the daemon's feed does not have yet.
///
/// The board is fetched on its own, on `r`, at start and on a view switch, so
/// it can show a move before the daemon's next read. Asked once per task:
/// a task only asks again after it leaves that state.
pub fn ask_for_auto_qa_check(state: &mut AppState) {
    let projects = state.config.qa_auto_projects();
    let stages: Vec<String> = state
        .config
        .qa_alerts()
        .stages
        .iter()
        .map(|stage| crate::daemon::normalise_stage(stage))
        .collect();
    let mut unseen = std::collections::BTreeSet::new();
    if let Some(board) = &state.board.board {
        for task in board
            .projects
            .values()
            .flat_map(|project| project.stages.values())
            .flat_map(|stage| stage.tasks.iter())
        {
            let auto = projects.iter().any(|project| {
                project
                    .trim()
                    .eq_ignore_ascii_case(task.project_name.trim())
            });
            let in_qa = stages.contains(&crate::daemon::normalise_stage(&task.stage_name));
            let known = state.board.auto_qa_tasks.contains_key(&task.id)
                || state
                    .board
                    .runs
                    .iter()
                    .any(|run| run.task_ids.contains(&task.id));
            if auto && in_qa && !known {
                unseen.insert(task.id);
            }
        }
    }
    let new: Vec<String> = unseen
        .difference(&state.auto_qa_unseen)
        .map(|id| format!("#{id}"))
        .collect();
    state.auto_qa_unseen = unseen;
    if new.is_empty() {
        return;
    }
    trace(&format!(
        "the board shows {} in QA before the daemon's feed does, so it asked for a check",
        new.join(", ")
    ));
    state.enqueue(Action::AutoQaCheck);
}

/// Auto QA's state in a few words, for the board's title. `None` when no
/// project has Auto QA on.
pub fn auto_qa_badge(state: &AppState, now: SystemTime) -> Option<String> {
    if state.config.qa_auto_projects().is_empty() {
        return None;
    }
    let Some(status) = &state.board.auto_qa_status else {
        return Some("Auto QA: no daemon feed".to_string());
    };
    if status.error.is_some() {
        return Some("Auto QA: check failed".to_string());
    }
    if status.watching.is_empty() && !status.paused.is_empty() {
        return Some("Auto QA: paused".to_string());
    }
    Some(match checked_ago(status, now) {
        Some(ago) => format!("Auto QA ✓ {ago}"),
        None => "Auto QA: not checked yet".to_string(),
    })
}

/// Auto QA's state in full, for the settings page. `None` when no project
/// has Auto QA on.
pub fn auto_qa_status_line(state: &AppState, now: SystemTime) -> Option<String> {
    if state.config.qa_auto_projects().is_empty() {
        return None;
    }
    let Some(status) = &state.board.auto_qa_status else {
        return Some(
            "No Auto QA feed from the daemon. It may be an older version: restart it.".to_string(),
        );
    };
    let mut parts = Vec::new();
    if !status.watching.is_empty() {
        parts.push(format!("Watching {}", status.watching.join(", ")));
    }
    if !status.paused.is_empty() {
        parts.push(format!(
            "another machine keeps {}",
            status.paused.join(", ")
        ));
    }
    let in_qa = state.board.auto_qa_tasks.len();
    if !status.watching.is_empty() {
        parts.push(format!(
            "{in_qa} task{} in QA",
            if in_qa == 1 { "" } else { "s" }
        ));
    }
    parts.push(match checked_ago(status, now) {
        Some(ago) => format!("checked {ago}"),
        None => "not checked yet".to_string(),
    });
    let mut line = parts.join(" · ");
    if let Some(first) = line.get(..1) {
        line.replace_range(..1, &first.to_uppercase());
    }
    if let Some(error) = &status.error {
        line.push_str(&format!(". The last check failed: {error}"));
    }
    Some(line)
}

/// "8s ago", from the feed's check time.
fn checked_ago(status: &AutoQaFeed, now: SystemTime) -> Option<String> {
    let checked = crate::util::parse_timestamp(&status.checked_at)?;
    let now: chrono::DateTime<chrono::Utc> = now.into();
    Some(crate::util::time_ago(checked, now.max(checked)))
}

/// Write one Auto QA step to `auto-qa.log`.
fn trace(message: &str) {
    crate::errorlog::trace(crate::errorlog::AUTO_QA_LOG, "auto-qa", message);
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
    trace(&format!(
        "run {run_id} has no coordinator, so one was started"
    ));
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
