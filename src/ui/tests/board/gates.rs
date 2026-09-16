//! Ported from `test/dependencies.test.js` and the duplicate-start half of
//! `test/task-session-link.test.js`.
//!
//! Two tasks on one real board were both blocked by a data-model task that had
//! not landed. Nothing stopped the dashboard starting them, which would have
//! produced plausible-looking merge requests against a schema that did not
//! exist yet. And nine agents once raced on one task in a single working tree,
//! each creating the same branch and overwriting the others' work.

use crate::ui::board::{gate_start, start, Gate, LaunchKind};
use crate::ui::dialogs::Dialog;
use crate::ui::state::Action;

use super::fixtures::*;

/// Whichever dialog the start flow opened, by its registry name.
fn opened(state: &crate::ui::state::AppState) -> Option<&'static str> {
    state.dialog.as_ref().map(Dialog::name)
}

#[test]
fn a_blocked_task_opens_the_dialog_instead_of_launching() {
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    assert_eq!(opened(&state), Some("blockedBy"));
    // If the gate leaked, resolving a directory is the next thing that happens.
    let queued = actions(&mut state);
    assert!(
        !queued.iter().any(|a| matches!(a, Action::Launch(_))),
        "the launch path ran anyway: {queued:?}"
    );
}

#[test]
fn a_task_whose_blockers_are_all_closed_is_not_gated() {
    // `blocker_count` 1 but none open — the blocker finished.
    let task = Task {
        open_blocker_count: 0,
        ..blocked_task(5238)
    };
    assert!(gate_start(&task, &LaunchKind::Task, std::iter::empty(), false).is_none());
}

#[test]
fn a_task_with_no_dependencies_at_all_is_not_gated() {
    let task = task(5238, "x");
    assert!(gate_start(&task, &LaunchKind::Task, std::iter::empty(), false).is_none());
}

#[test]
fn the_dialog_offers_the_override_because_odoo_is_sometimes_behind_reality() {
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    let painted = super::render_dialog(&mut state);
    assert!(painted.contains("#5238"), "{painted}");
    assert!(
        painted.contains("blocked by work that has not landed"),
        "{painted}"
    );
    assert!(painted.contains("y start anyway"), "{painted}");
    assert!(painted.contains("n / Esc cancel"), "{painted}");
}

#[test]
fn the_dialog_asks_odoo_for_the_blockers_rather_than_guessing() {
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    let queued = actions(&mut state);
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::FetchBlockers { task_id, blocker_ids }
                if *task_id == 5238 && blocker_ids == &vec![4034]
        )),
        "{queued:?}"
    );
}

#[test]
fn the_dialog_renders_when_the_blocker_lookup_fails() {
    // No Odoo in the test environment: it must still say something useful
    // rather than sitting on "loading blockers".
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    super::accept(
        &mut state,
        crate::ui::actions::BoardData::Failed {
            task_id: Some(5238),
            error: "no credentials".to_string(),
        },
    );
    let painted = super::render_dialog(&mut state);
    assert!(painted.contains("#5238"), "{painted}");
    assert!(
        !painted.contains("loading blockers"),
        "still showing the placeholder after the lookup settled: {painted}"
    );
    assert!(painted.contains("no credentials"), "{painted}");
}

#[test]
fn cancelling_does_not_dispatch() {
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    actions(&mut state);
    super::press(&mut state, crossterm::event::KeyCode::Esc);
    assert!(state.dialog.is_none());
    assert!(actions(&mut state).is_empty());
}

#[test]
fn y_forces_past_the_blocker_gate() {
    let (_dir, mut state) = board_state();
    // One saved folder, so forcing reaches a launch rather than a picker.
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    start(
        &mut state,
        start_request(&blocked_task(5238), LaunchKind::Task),
    );
    actions(&mut state);
    super::press(&mut state, crossterm::event::KeyCode::Char('y'));
    let queued = actions(&mut state);
    assert!(
        queued.iter().any(|a| matches!(a, Action::Launch(_))),
        "the override did not reach a launch: {queued:?}"
    );
}

// --- the duplicate-start guard ----------------------------------------------

#[test]
fn starting_a_task_that_already_has_a_session_is_intercepted() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("running", Some(5238), 1000)]);
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Task),
    );
    assert_eq!(opened(&state), Some("alreadyRunning"));
    let queued = actions(&mut state);
    assert!(!queued.iter().any(|a| matches!(a, Action::Launch(_))));
}

#[test]
fn it_reports_every_racing_session_not_just_the_first() {
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("a", Some(5238), 1000),
            live_session("b", Some(5238), 2000),
            live_session("c", Some(5238), 3000),
        ],
    );
    let gate = gate_start(&task(5238, "x"), &LaunchKind::Task, state.sessions(), false);
    match gate {
        Some(Gate::AlreadyRunning { running }) => assert_eq!(running.len(), 3),
        other => panic!("expected the duplicate guard, got {other:?}"),
    }
}

#[test]
fn two_sessions_in_one_worktree_are_flagged_red() {
    // The expensive case: same checkout means they overwrite each other's
    // branch and files. Different worktrees merely duplicate work.
    let (_dir, mut state) = board_state();
    let mut elsewhere = live_session("b", Some(5238), 2000);
    elsewhere.cwd = "/other-worktree".to_string();
    with_sessions(
        &mut state,
        vec![live_session("a", Some(5238), 1000), elsewhere],
    );
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Task),
    );
    let painted = super::render_dialog(&mut state);
    assert!(painted.contains("duplicate its work"), "{painted}");

    let (_dir, mut same) = board_state();
    with_sessions(
        &mut same,
        vec![
            live_session("a", Some(5238), 1000),
            live_session("b", Some(5238), 2000),
        ],
    );
    start(&mut same, start_request(&task(5238, "x"), LaunchKind::Task));
    let painted = super::render_dialog(&mut same);
    assert!(painted.contains("share a working tree"), "{painted}");
}

#[test]
fn a_task_with_no_live_session_starts_normally() {
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    with_sessions(&mut state, vec![live_session("other", Some(4036), 1000)]);
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Task),
    );
    assert!(state.dialog.is_none(), "a task with no session was blocked");
    let queued = actions(&mut state);
    assert!(
        queued.iter().any(|a| matches!(a, Action::Launch(_))),
        "the launch path was not reached: {queued:?}"
    );
}

#[test]
fn force_skips_the_guard_for_the_deliberate_second_run() {
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    with_sessions(&mut state, vec![live_session("running", Some(5238), 1000)]);
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Task).forced(),
    );
    assert!(state.dialog.is_none());
    assert!(actions(&mut state)
        .iter()
        .any(|a| matches!(a, Action::Launch(_))));
}

#[test]
fn a_dry_run_and_a_pre_work_brief_skip_the_duplicate_guard_on_purpose() {
    // A developer testing their own work legitimately has their own session
    // open on the task — the exact condition the guard exists to stop.
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("mine", Some(5238), 1000)]);
    for kind in [LaunchKind::QaDry, LaunchKind::PreOptics] {
        assert!(
            gate_start(&task(5238, "x"), &kind, state.sessions(), false).is_none(),
            "{kind:?} was gated"
        );
    }
    // A real QA pass keeps it: QA runs get started several at a time.
    assert!(gate_start(&task(5238, "x"), &LaunchKind::Qa, state.sessions(), false).is_some());
}

#[test]
fn only_the_task_pipeline_honours_odoos_blockers() {
    // A blocked task can still be reviewed, and often should be.
    let task = blocked_task(5238);
    for kind in [LaunchKind::Qa, LaunchKind::QaDry, LaunchKind::PreOptics] {
        let gate = gate_start(&task, &kind, std::iter::empty(), false);
        assert!(
            !matches!(gate, Some(Gate::BlockedBy { .. })),
            "{kind:?} was blocked by a dependency"
        );
    }
}

use crate::types::Task;
