//! The three worker actions the extras phase adds, at the spawn gate.
//!
//! Same two layers as [`super::launch`]: a refusing [`SpawnPolicy`] must stop
//! each of them before anything happens, and the recording driver proves no tab
//! was touched even if it did not.

use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;
use std::sync::Arc;

use crate::term::{SpawnPolicy, TerminalDriver};
use crate::ui::actions::ActionResult;
use crate::ui::board;
use crate::ui::state::Action;

use super::fixtures::*;
use super::launch::harness;

/// Ported from `test/purge.test.js`'s last block: the same guard every spawn
/// path has. A test that reached the real purge would SIGTERM the developer's
/// own running agents and close their tabs.
#[test]
fn purging_refuses_to_run_from_a_test_process() {
    let harness = harness();
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    let entries = vec![crate::purge::PurgeEntry {
        target: crate::purge::PurgeTarget {
            session_id: "s-1".into(),
            // A pid that is almost certainly not ours, so a regression here is
            // caught by the refusal rather than by killing something real.
            pids: vec![999_999],
            tty: Some("ttys001".into()),
            task_id: Some(9001),
        },
        stage: "Deployed".into(),
        kind: crate::purge::Stage::Done,
    }];
    crate::ui::actions::run_for_test(
        Action::Purge(Box::new(entries)),
        &driver,
        SpawnPolicy::Refuse,
        &harness.services,
        &tx,
    );
    drop(tx);
    let results: Vec<ActionResult> = rx.into_iter().collect();
    let refused = results
        .iter()
        .any(|result| matches!(result, ActionResult::Flash(message) if message.contains("Refusing to purge")));
    assert!(refused, "{results:?}");
    assert_eq!(harness.driver.closed.load(Ordering::SeqCst), 0);
}

#[test]
fn a_usage_check_under_a_refusing_policy_reports_rather_than_spending_quota() {
    let harness = harness();
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::run_for_test(
        Action::RefreshUsage,
        &driver,
        SpawnPolicy::Refuse,
        &harness.services,
        &tx,
    );
    drop(tx);
    let results: Vec<ActionResult> = rx.into_iter().collect();
    match results.first() {
        Some(ActionResult::Usage(snapshot)) => {
            assert!(!snapshot.ok);
            assert_eq!(snapshot.session, None, "a refusal is not a reading of zero");
        }
        other => panic!("expected a usage result, got {other:?}"),
    }
}

#[test]
fn a_qa_state_refresh_answers_with_a_label_even_when_git_cannot_run() {
    let harness = harness();
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::run_for_test(
        Action::RefreshQaState { task_id: 5238 },
        &driver,
        SpawnPolicy::Refuse,
        &harness.services,
        &tx,
    );
    drop(tx);
    let results: Vec<ActionResult> = rx.into_iter().collect();
    match results.first() {
        Some(ActionResult::Data(data)) => match data.as_ref() {
            crate::ui::actions::BoardData::QaLabel { task_id, label } => {
                assert_eq!(*task_id, 5238);
                assert_eq!(label, "🧪  QA this task");
            }
            other => panic!("expected a QA label, got {other:?}"),
        },
        other => panic!("expected a data result, got {other:?}"),
    }
}

// --- the two board slots the extras phase fills -----------------------------

#[test]
fn a_task_row_carries_the_optics_badge_and_the_real_auto_dev_marker() {
    // The two slots the board has held open since Phase 9a. The resolver is the
    // real one from `crate::autodev`, not a stub, so the tag map and the row
    // formatter are wired to each other.
    let (_dir, mut state) = board_state();
    let mut decorated = task(5944, "Recorded and driven");
    decorated.tags = vec!["auto_qa_fail".to_string()];
    with_tasks(&mut state, vec![decorated]);
    state.board.optics_tasks.insert(5944, 3);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));

    let row = board::window(&state, 0, 20)
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .find(|row| row.contains("#5944"))
        .expect("the task row");
    assert!(row.contains("🔬3"), "no Optics badge: {row}");
    assert!(row.contains("🤖✗ qa"), "no auto-dev marker: {row}");
}

#[test]
fn a_board_update_with_no_coverage_does_not_wipe_the_badges() {
    // Coverage is best-effort: a lookup that failed must leave what is on
    // screen alone rather than blanking every badge.
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5944, "x")]);
    state.board.optics_tasks.insert(5944, 2);
    with_tasks(&mut state, vec![task(5944, "x")]);
    assert_eq!(state.board.optics_tasks.get(&5944), Some(&2));
}

#[test]
fn the_detail_pane_names_recorded_optics_coverage() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5944, "Recorded")]);
    state.board.optics_tasks.insert(5944, 1);
    let one = crate::ui::board::detail::task_header(
        &task(5944, "Recorded"),
        crate::ui::board::detail::state_of(&state, 5944),
    );
    let text: String = one
        .iter()
        .map(|row| crate::board::plain_text(row))
        .collect();
    assert!(
        text.contains("🔬 1 recorded Optics process for this task"),
        "{text}"
    );

    state.board.optics_tasks.insert(5944, 4);
    let many = crate::ui::board::detail::task_header(
        &task(5944, "Recorded"),
        crate::ui::board::detail::state_of(&state, 5944),
    );
    let text: String = many
        .iter()
        .map(|row| crate::board::plain_text(row))
        .collect();
    assert!(text.contains("🔬 4 recorded Optics processes"), "{text}");

    // An install without Optics says nothing at all.
    state.board.optics_tasks.clear();
    let none = crate::ui::board::detail::task_header(
        &task(5944, "Recorded"),
        crate::ui::board::detail::state_of(&state, 5944),
    );
    let text: String = none
        .iter()
        .map(|row| crate::board::plain_text(row))
        .collect();
    assert!(!text.contains("🔬"), "{text}");
}
