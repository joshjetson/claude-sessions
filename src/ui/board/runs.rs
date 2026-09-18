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

    let mode = state.config.qa_coordinator_mode();
    match state.board.watch_stage(project, stage) {
        Some(covered) => {
            // Read here rather than in the slice, which holds no config. A run
            // fixes its mode at creation so its agreement number is not mixed
            // from two of them.
            if let Some(run) = state
                .board
                .runs
                .iter_mut()
                .find(|run| run.project_name == project && run.stage_name == stage)
            {
                run.mode = mode;
            }
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
        RunAction::StartRun => start_run(state, &command.run_id, &command.context),
        // Ask first, then start: the answer comes back as a StartRun carrying
        // whatever was typed.
        RunAction::StartRunWithContext => {
            let stage = state
                .board
                .runs
                .iter()
                .find(|run| run.id == command.run_id)
                .map(|run| run.stage_name.clone())
                .unwrap_or_default();
            state.dialog = Some(crate::ui::dialogs::Dialog::RunContext(
                crate::ui::dialogs::RunContext::new(&command.run_id, &stage),
            ));
            state.dirty = true;
        }
        RunAction::StopWatching => {
            if state.board.stop_watching(&command.run_id) {
                state.flash("Stopped watching. The QA sessions are untouched.".to_string());
            }
            state.dirty = true;
        }
        RunAction::Cancel => {}
    }
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

/// Start the run: the coordinator first, then the QA sessions.
///
/// The order matters, and the reason it does NOT have to be a wait is worth
/// writing down. A question is a row in the notifications table, and it stays
/// unresolved until something answers it. A coordinator that starts a moment
/// late finds the backlog waiting rather than missing it, so there is nothing
/// to synchronise on — only a gap to keep short, which starting it first does.
///
/// Re-pressing this on a run that already has a coordinator starts no second
/// one. Two coordinators would both triage the same questions.
fn start_run(state: &mut AppState, run_id: &str, extra_context: &str) {
    let already = state
        .board
        .runs
        .iter()
        .find(|run| run.id == run_id)
        .is_some_and(|run| run.coordinator_session.is_some() || run.coordinator_pending.is_some());

    if !already {
        start_coordinator(state, run_id, extra_context);
    }
    fill_lanes(state, run_id);
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
fn start_coordinator(state: &mut AppState, run_id: &str, extra_context: &str) {
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
    // Typed at launch, and told to outrank the coordinator's generic
    // instructions — the same contract every other pipeline gives it.
    if !extra_context.is_empty() {
        request = request.context(extra_context);
    }
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

    // Pin the folder before launching, so the coordinator can be recognised
    // afterwards by the folder it started in. Without a resolved folder the
    // launch still goes out — it opens a picker — and the run says plainly
    // that it could not track the result.
    let discovered = crate::ui::board::all_discovered_dirs(&state.discovered_dirs);
    let dir =
        match crate::ui::board::resolve_task_dir(&state.config, &task.project_name, &discovered) {
            crate::ui::board::DirChoice::Known(dir) => Some(dir),
            _ => None,
        };

    let pending = dir.as_ref().map(|dir| crate::qarun::CoordinatorPending {
        cwd: dir.clone(),
        known_session_ids: state.sessions().map(|s| s.session_id.clone()).collect(),
    });
    if let Some(dir) = dir.clone() {
        request = request.in_dir(dir);
    }

    if let Some(run) = state.board.runs.iter_mut().find(|run| run.id == run_id) {
        run.coordinator_pending = pending;
    }
    if dir.is_none() {
        state.flash(
            "Starting the coordinator, but its folder is not resolved yet — \
             the run cannot show its state until you pick one."
                .to_string(),
        );
    }

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
