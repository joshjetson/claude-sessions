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
use crate::ui::state::{Action, AppState};

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

/// Node's task pane listed the recorded workflows by name
/// (`controller.js:91-96`); the port shipped only the count. The list is its
/// own round trip, so the pane has to fold it in when it lands.
#[test]
fn the_recorded_processes_replace_the_count_when_the_lookup_lands() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5944, "Recorded")]);
    state.board.optics_tasks.insert(5944, 2);
    // Open the pane the way `→` does.
    open_task_pane(&mut state, 5944);
    assert_eq!(state.board.detail_answers.task_id, Some(5944));

    board::apply_result(
        &mut state,
        ActionResult::Data(Box::new(crate::ui::actions::BoardData::TaskOptics {
            task_id: 5944,
            optics: Some(optics_detail()),
        })),
    );
    let text = pane_text(&state);
    assert!(
        text.contains("🔬 Optics: 2 recorded processes under Task-5944 [orbital]"),
        "{text}"
    );
    assert!(text.contains("• Refund an order"), "{text}");
    assert!(text.contains("• Close the month"), "{text}");
    // The count line it replaces is gone, not repeated above it.
    assert!(!text.contains("for this task"), "{text}");
}

/// The description and the process list are separate round trips. Whichever
/// answers second must fold in beside the first rather than paint over it.
#[test]
fn a_description_and_the_process_list_survive_each_other_in_either_order() {
    for optics_first in [true, false] {
        let (_dir, mut state) = board_state();
        with_tasks(&mut state, vec![task(5944, "Recorded")]);
        open_task_pane(&mut state, 5944);

        let description =
            ActionResult::Data(Box::new(crate::ui::actions::BoardData::TaskDescription {
                task_id: 5944,
                detail: Some(crate::odoo::TaskDetail {
                    id: 5944,
                    description: "<p>Reconcile the ledger</p>".to_string(),
                    ..Default::default()
                }),
            }));
        let optics = ActionResult::Data(Box::new(crate::ui::actions::BoardData::TaskOptics {
            task_id: 5944,
            optics: Some(optics_detail()),
        }));
        let (first, second) = if optics_first {
            (optics, description)
        } else {
            (description, optics)
        };
        board::apply_result(&mut state, first);
        board::apply_result(&mut state, second);

        let text = pane_text(&state);
        assert!(
            text.contains("Reconcile the ledger"),
            "{optics_first}: {text}"
        );
        assert!(text.contains("• Refund an order"), "{optics_first}: {text}");
        assert!(
            !text.contains("loading description"),
            "{optics_first}: {text}"
        );
    }
}

/// A lookup that comes back after the cursor moved on belongs to nobody.
#[test]
fn a_process_list_for_another_task_never_lands_in_the_open_pane() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5944, "Recorded")]);
    open_task_pane(&mut state, 5944);
    board::apply_result(
        &mut state,
        ActionResult::Data(Box::new(crate::ui::actions::BoardData::TaskOptics {
            task_id: 9999,
            optics: Some(optics_detail()),
        })),
    );
    assert!(!pane_text(&state).contains("Refund an order"));
}

/// An install with no Optics endpoint never asks: the answer is always
/// "nothing recorded", and the lookup is a round trip.
#[test]
fn the_pane_only_asks_optics_when_the_install_has_optics() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5944, "Recorded")]);
    open_task_pane(&mut state, 5944);
    assert!(
        !state
            .take_actions()
            .iter()
            .any(|action| matches!(action, Action::FetchTaskOptics { .. })),
        "asked Optics without an endpoint"
    );
}

/// Node's task pane named the auto-dev daemon's state, its tag trail and its
/// run-log count (`controller.js:64-71`). Without it the only sign the daemon
/// has the task is a one-glyph badge on the row.
#[test]
fn the_detail_pane_reports_what_the_auto_dev_daemon_is_doing() {
    let (dir, mut state) = board_state();
    let mut tagged = task(6117, "Export the ledger");
    tagged.tags = vec!["auto_implemented".into(), "auto_sized".into()];
    with_tasks(&mut state, vec![tagged]);
    let runs = dir.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("implement-6117-20260901.log"), "…").unwrap();
    state.paths.auto_dev_runs_dir = runs;

    open_task_pane(&mut state, 6117);
    let text = pane_text(&state);
    assert!(text.contains("🤖 Auto-dev-daemon:"), "{text}");
    assert!(text.contains("tags: auto_implemented"), "{text}");
    assert!(text.contains("auto_sized"), "{text}");
    assert!(
        text.contains("1 run log → press D for daemon logs"),
        "{text}"
    );

    // And the menu row that opens them is there only because they exist.
    let menu =
        crate::ui::dialogs::TaskMenu::build(&state.board.task(6117).unwrap().clone(), &state);
    assert!(menu
        .entries
        .iter()
        .any(|(_, action)| *action == crate::ui::dialogs::TaskAction::DaemonLogs));
}

/// A task no daemon has touched says nothing about one, and offers no row for
/// logs that do not exist.
#[test]
fn a_task_with_no_auto_dev_tags_says_nothing_about_the_daemon() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(6118, "Hand-worked")]);
    open_task_pane(&mut state, 6118);
    assert!(!pane_text(&state).contains("Auto-dev-daemon"));
    let menu =
        crate::ui::dialogs::TaskMenu::build(&state.board.task(6118).unwrap().clone(), &state);
    assert!(!menu
        .entries
        .iter()
        .any(|(_, action)| *action == crate::ui::dialogs::TaskAction::DaemonLogs));
}

/// Put the cursor on a task row and press `→` until the pane opens, which is
/// the only way a task pane is ever opened.
fn open_task_pane(state: &mut AppState, task_id: i64) {
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    let snapshot = board::snapshot(state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key == &format!("bt:{task_id}"))
        .expect("the task row");
    state.board_sel.set(&snapshot.keys, index);
    super::press(state, crossterm::event::KeyCode::Right);
    super::press(state, crossterm::event::KeyCode::Right);
}

/// And an install that has one asks every time the pane opens, the way Node
/// did — the count on the board comes from a per-project query that may predate
/// the recording someone made a minute ago.
#[test]
fn a_configured_install_asks_optics_for_every_task_pane() {
    let (dir, mut state) = board_state();
    std::fs::write(
        dir.path().join(".claude-sessions.json"),
        r#"{"optics":{"api":"https://optics.example.com","token":"t"}}"#,
    )
    .unwrap();
    state.config = crate::config::ConfigHandle::load(
        &state.paths.clone(),
        crate::config::EnvOverrides::default(),
    );
    with_tasks(&mut state, vec![task(5944, "Recorded")]);
    open_task_pane(&mut state, 5944);
    assert!(
        state.take_actions().iter().any(|action| matches!(
            action,
            Action::FetchTaskOptics { task_id: 5944, project } if project == "NoSuchProject-ForTests"
        )),
        "the pane never asked Optics"
    );
}

fn optics_detail() -> crate::optics::TaskOptics {
    crate::optics::TaskOptics {
        project_sdk_key: "orbital".into(),
        category: "Task-5944".into(),
        category_id: 7,
        processes: vec![
            crate::optics::OpticsProcess {
                id: 1,
                name: "Refund an order".into(),
                ..Default::default()
            },
            crate::optics::OpticsProcess {
                id: 2,
                name: "Close the month".into(),
                ..Default::default()
            },
        ],
    }
}

fn pane_text(state: &AppState) -> String {
    state
        .board
        .detail
        .as_ref()
        .expect("a pane to be open")
        .rows
        .iter()
        .map(|row| format!("{}\n", crate::board::plain_text(row)))
        .collect()
}
