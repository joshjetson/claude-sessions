//! Admission — the part of a run a model never gets to decide.
//!
//! Every assertion here is a refusal. That is the shape of the thing: the
//! scheduler's whole job is to say no in the cases where saying yes breaks
//! something, and to name which no it said, because a run that silently starts
//! nothing is indistinguishable from a run that is merely slow.

use std::collections::HashSet;

use super::run;
use crate::qaden::{QaRunState, QaVerdict};
use crate::qarun::{admit, first_refusal, plan_spawns, AdmitCtx, Refusal};

fn nothing_recorded(_task_id: i64) -> QaRunState {
    QaRunState::default()
}

fn ctx<'a>(live: &'a HashSet<i64>, limit: Option<usize>) -> AdmitCtx<'a> {
    AdmitCtx {
        live_task_ids: live,
        state_of: &nothing_recorded,
        lane_limit: limit,
    }
}

#[test]
fn a_fresh_task_is_admitted_when_lanes_are_free() {
    let live = HashSet::new();
    assert_eq!(admit(&run(vec![1, 2]), 1, &ctx(&live, Some(4))), Ok(()));
}

#[test]
fn a_task_outside_the_run_is_refused() {
    let live = HashSet::new();
    assert_eq!(
        admit(&run(vec![1]), 99, &ctx(&live, None)),
        Err(Refusal::NotInRun(99))
    );
}

#[test]
fn a_task_with_a_live_session_is_refused() {
    // Two passes on one task share a worktree path, and the worktree setup
    // tears the existing checkout down to rebuild it — mid-review.
    let live = HashSet::from([1]);
    assert_eq!(
        admit(&run(vec![1]), 1, &ctx(&live, None)),
        Err(Refusal::AlreadyRunning(1))
    );
}

#[test]
fn a_task_this_run_already_started_is_refused() {
    let live = HashSet::new();
    let mut started = run(vec![1]);
    started.spawned.push(1);
    assert_eq!(
        admit(&started, 1, &ctx(&live, None)),
        Err(Refusal::AlreadySpawned(1))
    );
}

#[test]
fn a_task_that_reached_a_verdict_is_refused() {
    // Starting again archives a completed round.
    let live = HashSet::new();
    let finished = |_id: i64| QaRunState {
        exists: true,
        verdict: Some(QaVerdict::Pass),
        ..QaRunState::default()
    };
    let ctx = AdmitCtx {
        live_task_ids: &live,
        state_of: &finished,
        lane_limit: None,
    };
    assert_eq!(
        admit(&run(vec![1]), 1, &ctx),
        Err(Refusal::AlreadyFinished(1, QaVerdict::Pass))
    );
}

#[test]
fn the_lane_limit_refuses_and_says_how_full() {
    let live = HashSet::from([1, 2, 3, 4]);
    assert_eq!(
        admit(&run(vec![1, 2, 3, 4, 5]), 5, &ctx(&live, Some(4))),
        Err(Refusal::LaneLimit {
            running: 4,
            limit: 4
        })
    );
}

#[test]
fn an_uncapped_run_never_refuses_on_concurrency() {
    // The default. A cap below someone's normal working volume obstructs rather
    // than protects, and one pass per task is enforced separately.
    let live: HashSet<i64> = (1..=59).collect();
    let ids: Vec<i64> = (1..=60).collect();
    assert_eq!(admit(&run(ids), 60, &ctx(&live, None)), Ok(()));
}

#[test]
fn every_refusal_carries_a_readable_detail() {
    for refusal in [
        Refusal::NotInRun(9),
        Refusal::AlreadyRunning(1),
        Refusal::AlreadySpawned(1),
        Refusal::AlreadyFinished(1, QaVerdict::Revisions),
        Refusal::LaneLimit {
            running: 4,
            limit: 4,
        },
    ] {
        assert!(refusal.detail().len() > 10, "thin refusal: {refusal:?}");
    }
}

#[test]
fn a_plan_fills_the_free_lanes_and_stops() {
    let live = HashSet::new();
    assert_eq!(
        plan_spawns(&run(vec![10, 11, 12, 13, 14]), &ctx(&live, Some(3))),
        vec![10, 11, 12]
    );
}

#[test]
fn a_plan_counts_each_planned_task_against_the_limit() {
    // Without that, one pass hands the same single free lane to every remaining
    // task and starts all of them.
    let live = HashSet::new();
    assert_eq!(
        plan_spawns(&run(vec![1, 2, 3, 4]), &ctx(&live, Some(2))).len(),
        2
    );
}

#[test]
fn a_plan_accounts_for_sessions_already_running() {
    let live = HashSet::from([30, 31]);
    assert_eq!(
        plan_spawns(&run(vec![30, 31, 32, 33]), &ctx(&live, Some(3))),
        vec![32]
    );
}

#[test]
fn a_full_run_plans_nothing() {
    let live = HashSet::from([1, 2]);
    assert!(plan_spawns(&run(vec![1, 2, 3]), &ctx(&live, Some(2))).is_empty());
}

#[test]
fn a_finished_task_is_skipped_without_spending_a_lane() {
    let live = HashSet::new();
    let done_first_two = |id: i64| {
        if id <= 51 {
            QaRunState {
                exists: true,
                verdict: Some(QaVerdict::Pass),
                ..QaRunState::default()
            }
        } else {
            QaRunState::default()
        }
    };
    let ctx = AdmitCtx {
        live_task_ids: &live,
        state_of: &done_first_two,
        lane_limit: Some(2),
    };
    assert_eq!(plan_spawns(&run(vec![50, 51, 52, 53]), &ctx), vec![52, 53]);
}

#[test]
fn a_plan_keeps_the_runs_own_order() {
    // The run's list is the order the board showed before sorting, and a plan
    // that reordered it would start tasks in an order nobody chose.
    let live = HashSet::new();
    assert_eq!(
        plan_spawns(&run(vec![63, 61, 62]), &ctx(&live, Some(3))),
        vec![63, 61, 62]
    );
}

#[test]
fn planning_does_not_mutate_the_run() {
    let live = HashSet::new();
    let subject = run(vec![70, 71]);
    plan_spawns(&subject, &ctx(&live, None));
    assert!(subject.spawned.is_empty(), "planning is not starting");
    assert_eq!(subject.task_ids, vec![70, 71]);
}

#[test]
fn an_empty_run_plans_nothing() {
    let live = HashSet::new();
    assert!(plan_spawns(&run(vec![]), &ctx(&live, None)).is_empty());
}

#[test]
fn the_first_refusal_is_what_a_caller_reports() {
    // An empty plan has several possible causes and they call for different
    // responses, so the caller has to be able to say which one it was.
    let live = HashSet::from([1, 2]);
    assert_eq!(
        first_refusal(&run(vec![1, 2]), &ctx(&live, Some(2))),
        Some(Refusal::AlreadyRunning(1))
    );
}
