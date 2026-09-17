//! The ring buffer, the events, and the refusals — nothing here spawns.

use serde_json::json;

use super::{engine_with_board, seed_run, task, PROJECT};
use crate::daemon::tests::{engine_with, Setup};
use crate::daemon::{DeployRunState, MAX_RUN_LINES, WIRE_RUN_LINES};
use crate::odoo::task_state;

// --- the ring buffer ---------------------------------------------------------

#[test]
fn the_ring_buffer_keeps_the_last_two_thousand_and_ships_the_last_two_hundred() {
    // A docker build emits tens of thousands of lines; the pane shows a
    // screenful, and holding the whole log for hours is what grows the heap.
    let mut run = DeployRunState::started("./deploy.sh", Some(42));
    for index in 0..(MAX_RUN_LINES + 500) {
        run.push(format!("line {index}"));
    }

    let held = run.log();
    assert_eq!(held.len(), MAX_RUN_LINES);
    assert_eq!(held[0], "line 500", "the oldest lines fell off the front");
    assert_eq!(
        held[MAX_RUN_LINES - 1],
        format!("line {}", MAX_RUN_LINES + 499)
    );

    let wire = run.wire(PROJECT);
    assert_eq!(wire.lines.len(), WIRE_RUN_LINES);
    assert_eq!(
        wire.lines[0],
        format!("line {}", MAX_RUN_LINES + 500 - WIRE_RUN_LINES)
    );
    // The total counts everything ever emitted, so a pane can say "the last
    // 200 of 2,500" rather than implying that is all there was.
    assert_eq!(wire.total_lines, MAX_RUN_LINES + 500);
    assert_eq!(wire.project, PROJECT);
    assert_eq!(wire.command, "./deploy.sh");
}

#[test]
fn a_short_run_ships_everything_it_has() {
    let mut run = DeployRunState::started("./deploy.sh", None);
    run.push("building".to_string());
    run.push("done".to_string());
    let wire = run.wire(PROJECT);
    assert_eq!(wire.lines, ["building", "done"]);
    assert_eq!(wire.total_lines, 2);
    assert!(wire.is_running());
}

// --- events -------------------------------------------------------------------

#[test]
fn every_output_line_and_every_state_change_reaches_a_subscriber() {
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    let events = harness.engine.subscribe();

    harness
        .inner()
        .push_deploy_line(PROJECT, "building".to_string());
    harness.inner().finish_deploy(PROJECT, Some(0));

    let names: Vec<&'static str> = events.try_iter().map(|event| event.name()).collect();
    assert!(names.contains(&"deploy-output"), "{names:?}");
    assert!(names.contains(&"deploy-run"), "{names:?}");
    // The board was re-read, so clients are told it changed.
    assert!(names.contains(&"deploy"), "{names:?}");
}

#[test]
fn the_snapshot_carries_the_board_and_every_run() {
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    harness.inner().poll_deploy();

    let snapshot = harness.engine.snapshot();
    assert_eq!(snapshot.deploy.as_ref().unwrap().project_names, [PROJECT]);
    assert!(snapshot.deploy_error.is_none());
    assert!(snapshot.deploy_runs.contains_key(PROJECT));
    // Round-trips, because it crosses a socket as JSON.
    let json = serde_json::to_string(&snapshot).expect("serialised");
    assert!(json.contains("deployRuns"), "camelCase on the wire");
}

#[test]
fn a_failed_fetch_keeps_the_previous_board_and_says_why() {
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    harness.inner().poll_deploy();
    assert!(harness.state().deploy.is_some());

    // Replace the state directly: a stale list with an error beside it beats
    // an empty tab.
    harness.state().deploy_error = Some("glab: command not found".to_string());
    let snapshot = harness.engine.snapshot();
    assert!(snapshot.deploy.is_some());
    assert_eq!(
        snapshot.deploy_error.as_deref(),
        Some("glab: command not found")
    );
}

// --- refusals -----------------------------------------------------------------

#[test]
fn starting_a_deploy_is_refused_with_a_reason_rather_than_failing() {
    let harness = engine_with(Setup {
        config: Some(json!({ "deploy": { "projects": { PROJECT: { "command": "" } } } })),
        ..Setup::default()
    });

    let unknown = harness.engine.start_deploy("Nowhere");
    assert!(!unknown.ok);
    assert!(unknown.error.unwrap().contains("not configured for deploy"));

    let no_command = harness.engine.start_deploy(PROJECT);
    assert!(!no_command.ok);
    assert!(no_command
        .error
        .unwrap()
        .contains("No deploy command configured"));
}

#[test]
fn a_second_deploy_of_the_same_project_is_refused_while_one_is_running() {
    let (harness, _) = engine_with_board(Vec::new());
    seed_run(&harness);
    let result = harness.engine.start_deploy(PROJECT);
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("Aurora is already deploying.")
    );
}

#[test]
fn a_refusing_spawn_policy_stops_a_deploy_before_any_process_starts() {
    // The guard the whole crate funnels through: a test run must never be able
    // to fire a production deploy.
    let (harness, _) = engine_with_board(Vec::new());
    let result = harness.engine.start_deploy(PROJECT);
    assert!(!result.ok);
    let message = result.error.unwrap();
    assert!(message.contains("Refusing to deploy Aurora"), "{message}");
    assert!(
        harness.state().deploy_runs.is_empty(),
        "nothing was recorded"
    );
}

#[test]
fn cancelling_nothing_says_so() {
    let (harness, _) = engine_with_board(Vec::new());
    let result = harness.engine.cancel_deploy(PROJECT);
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("No running deploy for Aurora.")
    );

    // A finished run is not a running one.
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    assert!(!harness.engine.cancel_deploy(PROJECT).ok);
}

#[test]
fn the_log_route_reads_the_whole_buffer_and_an_unknown_project_is_empty() {
    let (harness, _) = engine_with_board(Vec::new());
    seed_run(&harness);
    for index in 0..300 {
        harness
            .inner()
            .push_deploy_line(PROJECT, format!("line {index}"));
    }
    assert_eq!(harness.engine.deploy_log(PROJECT).len(), 300);
    assert_eq!(
        harness.engine.deploy_run(PROJECT).unwrap().lines.len(),
        WIRE_RUN_LINES,
        "the wire form is still capped"
    );
    assert!(harness.engine.deploy_log("Nowhere").is_empty());
}

#[test]
fn a_daemon_with_no_deploy_hook_answers_nothing_rather_than_erroring() {
    let harness = engine_with(Setup::default());
    harness.engine.refresh_deploy_board();
    harness.engine.join_workers();
    assert!(harness.state().deploy.is_none());
}
