//! What a finished run does to Odoo: only exit 0, only merged MRs, never the
//! stage.

use std::sync::atomic::Ordering;

use super::{engine_with_board, run_lines, seed_run, task, PROJECT};
use crate::daemon::tests::fakes::BackendCall;
use crate::odoo::task_state;
use crate::types::DeployRunStatus;

// --- what a finished run does ------------------------------------------------

#[test]
fn a_clean_deploy_marks_only_the_merged_tasks_complete() {
    let (harness, fetches) = engine_with_board(vec![
        task(1, Some("merged"), task_state::IN_PROGRESS),
        task(2, Some("opened"), task_state::IN_PROGRESS),
        task(3, None, task_state::CHANGES_REQUESTED),
    ]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));

    assert_eq!(
        harness.backend.state_writes(),
        vec![(1, task_state::COMPLETE.to_string())],
        "only the merged MR shipped"
    );
    // The board is re-read first: "did this ship" is GitLab's answer, and the
    // board loaded before the run predates every merge it depended on.
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    let report = run_lines(&harness).join("\n");
    assert!(
        report.contains("Marking 1 shipped task(s) Complete"),
        "{report}"
    );
    assert!(report.contains("✓ #1"), "{report}");
    assert!(
        report.contains("– #2 left alone (opened — not shipped)"),
        "{report}"
    );
    assert!(
        report.contains("– #3 left alone (no MR — not shipped)"),
        "{report}"
    );
}

#[test]
fn a_deploy_never_touches_the_stage() {
    // The task stays in Deployed. Moving it would be the dashboard deciding
    // something only a human should.
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    assert!(
        !harness
            .backend
            .calls()
            .iter()
            .any(|call| matches!(call, BackendCall::MoveStage(_))),
        "the stage was moved"
    );
}

#[test]
fn a_failed_deploy_closes_nothing_and_does_not_even_re_read_the_board() {
    let (harness, fetches) =
        engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(1));

    assert!(
        harness.backend.state_writes().is_empty(),
        "a failure shipped nothing"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
    let run = harness.engine.deploy_run(PROJECT).expect("a run");
    assert_eq!(run.status, DeployRunStatus::Fail);
    assert_eq!(run.exit_code, Some(1));
}

#[test]
fn a_deploy_killed_by_a_signal_is_a_failure_too() {
    // `None` is what a signalled child reports — the cancel path's outcome.
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, None);
    let run = harness.engine.deploy_run(PROJECT).expect("a run");
    assert_eq!(run.status, DeployRunStatus::Fail);
    assert!(harness.backend.state_writes().is_empty());
}

#[test]
fn a_task_already_marked_complete_is_not_written_again() {
    let (harness, _) = engine_with_board(vec![
        task(1, Some("merged"), task_state::COMPLETE),
        task(2, Some("merged"), task_state::IN_PROGRESS),
    ]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    assert_eq!(
        harness.backend.state_writes(),
        vec![(2, task_state::COMPLETE.to_string())]
    );
}

#[test]
fn nothing_merged_is_reported_rather_than_passing_in_silence() {
    let (harness, _) = engine_with_board(vec![task(1, Some("opened"), task_state::IN_PROGRESS)]);
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    assert!(harness.backend.state_writes().is_empty());
    let report = run_lines(&harness).join("\n");
    assert!(
        report.contains("1 task(s) left as-is: their MRs aren't merged"),
        "{report}"
    );
}

#[test]
fn an_empty_deployed_stage_says_nothing_at_all() {
    let (harness, _) = engine_with_board(Vec::new());
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    assert!(run_lines(&harness).is_empty(), "no report for no tasks");
}

#[test]
fn an_odoo_refusal_is_reported_and_the_run_still_finishes() {
    let (harness, _) = engine_with_board(vec![task(1, Some("merged"), task_state::IN_PROGRESS)]);
    *harness.backend.state_error.lock().unwrap() = Some("Access denied".to_string());
    seed_run(&harness);
    harness.inner().finish_deploy(PROJECT, Some(0));
    let report = run_lines(&harness).join("\n");
    assert!(report.contains("✗ #1 failed: Access denied"), "{report}");
    assert_eq!(
        harness.engine.deploy_run(PROJECT).unwrap().status,
        DeployRunStatus::Ok,
        "the deploy itself succeeded"
    );
}
