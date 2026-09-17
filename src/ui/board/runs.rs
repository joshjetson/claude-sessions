//! The board's QA-run commands: watching a stage, and acting on a run.
//!
//! Starting sessions and starting a coordinator both go through the shared
//! launch path, so a run is a way of starting several passes rather than a
//! different kind of pass. If the prompts diverged, a run's results would not be
//! comparable with a pass someone started by hand.

use crate::qarun::{first_refusal, plan_spawns, AdmitCtx, QaRun, RunMode};
use crate::ui::dialogs::{RunAction, RunCommand};
use crate::ui::state::AppState;

/// Start watching the stage the cursor is on, or drop the run it is on.
///
/// Read-only either way: it groups rows and reports, and starts nothing.
pub fn watch_or_drop(state: &mut AppState, project: &str, stage: &str, existing: Option<String>) {
    if let Some(run_id) = existing {
        if state.board.stop_watching(&run_id) {
            state.flash("Stopped watching that run. The QA sessions are untouched.".to_string());
        }
        state.dirty = true;
        return;
    }

    match state.board.watch_stage(project, stage) {
        Some(covered) => {
            // Open the stage, or the run appears under a collapsed header and
            // looks like nothing happened.
            let key = crate::board::stage_key(project, stage);
            state.board.set_expanded(&key, true);
            state.flash(format!(
                "Watching {covered} task{} in {stage} as a QA run.",
                if covered == 1 { "" } else { "s" }
            ));
        }
        None => state.flash("That stage has no tasks to watch.".to_string()),
    }
    state.dirty = true;
}

/// Act on a run.
pub fn run_command(state: &mut AppState, command: RunCommand) {
    match command.action {
        RunAction::FillLanes => fill_lanes(state, &command.run_id),
        RunAction::StartCoordinator => start_coordinator(state, &command.run_id),
        RunAction::ToggleMode => toggle_mode(state, &command.run_id),
        RunAction::StopWatching => {
            if state.board.stop_watching(&command.run_id) {
                state.flash("Stopped watching. The QA sessions are untouched.".to_string());
            }
            state.dirty = true;
        }
        RunAction::Cancel => {}
    }
}

/// Flip shadow ↔ triage.
///
/// Takes effect on the NEXT coordinator, not one already running: its rules are
/// in a prompt that has already been sent. Saying so is the point — a toggle
/// that looked instant while the live session carried the old rules would be
/// worse than no toggle.
fn toggle_mode(state: &mut AppState, run_id: &str) {
    let Some(run) = state.board.runs.iter_mut().find(|run| run.id == run_id) else {
        return;
    };
    run.mode = match run.mode {
        RunMode::Shadow => RunMode::Triage,
        RunMode::Triage => RunMode::Shadow,
    };
    let now = match run.mode {
        RunMode::Shadow => "shadow — it will record answers and give none",
        RunMode::Triage => "triage — it will answer questions of fact",
    };
    state.flash(format!(
        "Next coordinator runs in {now}. The one already running keeps the rules it was started with."
    ));
    state.dirty = true;
}

/// Which tasks in a run currently have a live session.
fn live_task_ids(state: &AppState, run: &QaRun) -> std::collections::HashSet<i64> {
    run.task_ids
        .iter()
        .copied()
        .filter(|&task_id| {
            crate::ui::board::task_session(state.sessions(), task_id, state.board.link(task_id))
                .is_some()
        })
        .collect()
}

/// Start the QA sessions the run has lanes for.
///
/// Admission is the scheduler's decision, not this function's. Every refusal is
/// surfaced: a run that quietly starts nothing looks exactly like a run that is
/// merely slow.
fn fill_lanes(state: &mut AppState, run_id: &str) {
    let Some(run) = state
        .board
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .cloned()
    else {
        return;
    };
    let live = live_task_ids(state, &run);
    let paths = state.paths.clone();
    let state_of = move |task_id: i64| crate::qaden::qa_run_state(&paths, task_id, |_| None);
    let ctx = AdmitCtx {
        live_task_ids: &live,
        state_of: &state_of,
        lane_limit: state.config.qa_lane_limit(),
    };

    let plan = plan_spawns(&run, &ctx);
    if plan.is_empty() {
        let why = first_refusal(&run, &ctx)
            .map(|refusal| refusal.detail())
            .unwrap_or_else(|| "Every task is finished or already running.".to_string());
        state.flash(format!("Nothing to start: {why}"));
        state.dirty = true;
        return;
    }

    // Recorded here rather than inside the loop, so a second press cannot plan
    // the same tasks again while the first batch is still opening terminals.
    if let Some(run) = state.board.runs.iter_mut().find(|run| run.id == run_id) {
        run.spawned.extend(plan.iter().copied());
    }

    state.flash(format!(
        "Starting {} QA session{}…",
        plan.len(),
        if plan.len() == 1 { "" } else { "s" }
    ));
    for task_id in plan {
        if let Some(task) = state.board.task(task_id).cloned() {
            crate::ui::board::start(
                state,
                crate::ui::board::StartRequest::new(&task, crate::ui::board::LaunchKind::Qa),
            );
        }
    }
    state.dirty = true;
}

/// Start the coordinating session for a run.
///
/// It watches. It does not spawn — admission lives outside any model — and in
/// shadow mode it answers nothing.
fn start_coordinator(state: &mut AppState, run_id: &str) {
    let Some(run) = state
        .board
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .cloned()
    else {
        return;
    };
    // The coordinator belongs to the run, not to any one task, so it is
    // launched against the run's first task only to resolve the project folder.
    let Some(task) = run
        .task_ids
        .first()
        .and_then(|id| state.board.task(*id))
        .cloned()
    else {
        state.flash(
            "This run has no task the board still knows, so its folder cannot be resolved."
                .to_string(),
        );
        state.dirty = true;
        return;
    };

    let mut request =
        crate::ui::board::StartRequest::new(&task, crate::ui::board::LaunchKind::QaRun);
    request.extras.insert(
        crate::pipeline::definitions::RUN_ID_VAR.to_string(),
        run.id.clone(),
    );
    request.extras.insert(
        crate::pipeline::definitions::TASK_IDS_VAR.to_string(),
        run.task_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );
    request.extras.insert(
        crate::pipeline::definitions::QA_ROOT_VAR.to_string(),
        state.paths.qa_root.display().to_string(),
    );
    request.extras.insert(
        crate::pipeline::definitions::TRIAGE_VAR.to_string(),
        (run.mode == RunMode::Triage).to_string(),
    );

    crate::ui::board::start(state, request);
    state.dirty = true;
}

/// Move the cursor to the next task waiting on the reviewer, wrapping.
///
/// Wrapping rather than stopping, so pressing it repeatedly cycles instead of
/// sticking on the first one. With seven rows open, scrolling to find the
/// flashing one is the thing this key exists to replace.
pub fn jump_to_next_ask(state: &mut AppState, keys: &[String]) {
    let sessions = std::collections::HashMap::new();
    let now = std::time::SystemTime::now();
    let next = {
        let ctx = state
            .board
            .run_ctx(&state.paths, &state.notifications, &sessions, now);
        crate::qarun::next_ask_key(&state.board.runs, &ctx, state.board_sel.key())
    };

    match next {
        Some(key) if state.board_sel.select_key(keys, &key) => {}
        Some(_) => state.flash(
            "That row is not on screen — open the run, or the stage it sits under.".to_string(),
        ),
        None => state.flash("No agent is waiting on you.".to_string()),
    }
    state.dirty = true;
}
