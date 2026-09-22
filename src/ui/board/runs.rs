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
        state.save_runs();
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
    state.save_runs();
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
            state.save_runs();
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
        .is_some_and(|run| run.coordinator_started);

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
    fill_lanes_inner(state, run_id, true)
}

/// Start whatever lanes are free, without saying anything when there is nothing
/// to start. Used by the automatic refill, which runs on a timer: "Nothing to
/// start: every task is finished" is true and correct once, and noise forever.
fn fill_lanes_quiet(state: &mut AppState, run_id: &str) {
    fill_lanes_inner(state, run_id, false)
}

fn fill_lanes_inner(state: &mut AppState, run_id: &str, announce: bool) {
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
        if announce {
            let why = first_refusal(&run, &ctx)
                .map(|refusal| refusal.detail())
                .unwrap_or_else(|| "Every task is finished or already running.".to_string());
            state.flash(format!("Nothing to start: {why}"));
            state.dirty = true;
        }
        return;
    }

    if announce {
        state.flash(format!(
            "Starting {} QA session{}…",
            plan.len(),
            if plan.len() == 1 { "" } else { "s" }
        ));
    }

    // A task counts as SPAWNED once its launch actually goes out, never when it
    // is merely planned.
    //
    // It used to be recorded up front, to stop a second press planning the same
    // tasks while the first batch was still opening terminals. But `admit`
    // refuses an already-spawned task forever, so a launch that never opened
    // was unstartable for the life of the run: fourteen tasks were marked
    // started, one terminal appeared, and the other thirteen could not be
    // retried by any means short of rebuilding the run. The rows read "not
    // running" and there was no way back.
    //
    // The double-start it was guarding against is covered anyway —
    // `Refusal::AlreadyRunning` refuses any task that has a live session, which
    // is what a task whose terminal did open will have.
    let mut started = Vec::new();
    let mut failed = Vec::new();
    for task_id in plan {
        let Some(task) = state.board.task(task_id).cloned() else {
            // The board no longer knows this task, so nothing was launched.
            failed.push(task_id);
            continue;
        };

        // A start does not always launch: it can open a folder picker or a
        // confirmation instead, and both can be cancelled. Only a launch that
        // actually reached the queue may be recorded as spawned — recording the
        // others would make a cancelled picker permanently unstartable, which
        // is the fault this whole change exists to remove.
        let before = state.queued_launches();
        crate::ui::board::start(
            state,
            crate::ui::board::StartRequest::new(&task, crate::ui::board::LaunchKind::Qa),
        );
        if state.queued_launches() > before {
            started.push(task_id);
        } else {
            failed.push(task_id);
        }
    }

    if let Some(run) = state.board.runs.iter_mut().find(|run| run.id == run_id) {
        run.spawned.extend(started.iter().copied());
    }
    if !started.is_empty() {
        state.last_run_launch = Some(std::time::SystemTime::now());
    }
    state.save_runs();

    // Said out loud. A run that starts eleven of fourteen and reports "Starting
    // 14…" is how thirteen tasks went missing without a line anywhere.
    if !failed.is_empty() {
        state.flash(format!(
            "Started {}, but {} could not be launched and stay startable: {}",
            started.len(),
            failed.len(),
            failed
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
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

    // The folder no longer has to be pinned for recognition — the run id in
    // the environment does that — so this launch resolves its directory the
    // same way every other one does, picker and all.
    if let Some(run) = state.board.runs.iter_mut().find(|run| run.id == run_id) {
        run.coordinator_started = true;
    }
    state.save_runs();

    crate::ui::board::start(state, request);
    state.dirty = true;
}

/// How long a run stays quiet after a launch before the refill looks again.
///
/// A launched session takes a second or two to appear in a scan. Until it does
/// it is not in the live set, so its lane still reads free — and a refill that
/// ran immediately would hand that same lane to the next task, and the next,
/// until the whole queue was started at once. That is the behaviour the lane
/// limit exists to prevent, arrived at from the other direction.
///
/// `spawned` stops a task being started twice, so the worst this window
/// prevents is overshooting the LIMIT, not double-starting a task.
const REFILL_QUIET: std::time::Duration = std::time::Duration::from_secs(20);

/// Start the next task in every started run that has a free lane.
///
/// Called on each scan, because a lane frees when a session ENDS and nothing
/// else notices that. Without it a limit of four means pressing "Start run"
/// four separate times for fourteen tasks, which is why the limit was set to
/// uncapped and thirty-five agents ended up resident at once — about 405 MB
/// each, on a machine with sixteen gigabytes.
///
/// The point is not to cap the work. It is to hold the number of SIMULTANEOUS
/// agents flat while the queue drains, which is the difference between fitting
/// in memory and paging.
pub fn auto_refill(state: &mut AppState, now: std::time::SystemTime) {
    if !state.config.qa_auto_refill() {
        return;
    }
    if let Some(last) = state.last_run_launch {
        if now.duration_since(last).unwrap_or_default() < REFILL_QUIET {
            return;
        }
    }

    // Only runs the reviewer actually STARTED. Watching a stage groups its rows
    // and starts nothing, and a refill that launched into a merely-watched run
    // would start work nobody asked for.
    let started: Vec<String> = state
        .board
        .runs
        .iter()
        .filter(|run| run.coordinator_started)
        .map(|run| run.id.clone())
        .collect();

    for run_id in started {
        fill_lanes_quiet(state, &run_id);
    }
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
