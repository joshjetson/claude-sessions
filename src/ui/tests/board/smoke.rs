//! The smoke test launch: QAden's `/smoke-test` from the task menu.
//!
//! A smoke test names its task like any session, so the board and the Sessions
//! tab show its number. It is not a QA pass: it must not stop QA from starting,
//! must not stand in for a run's QA session, and must not receive a revision
//! meant for the session that wrote the code. A second smoke test on the same
//! task is refused like a second QA pass.

use std::sync::mpsc::channel;
use std::sync::Arc;

use crate::term::{SpawnPolicy, TerminalDriver, LAUNCH_KIND_ENV, TASK_ID_ENV};
use crate::types::{Session, UserRole};
use crate::ui::board::{gate_start, start, Gate, LaunchKind};
use crate::ui::dialogs::{TaskAction, TaskMenu};
use crate::ui::state::Action;

use super::fixtures::*;

fn smoke_session(id: &str, task_id: i64, mtime: u64) -> Session {
    Session {
        launch_kind: Some("smoke".to_string()),
        ..live_session(id, Some(task_id), mtime)
    }
}

#[test]
fn a_live_smoke_test_does_not_gate_qa_or_development() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![smoke_session("smoke", 5238, 1000)]);
    for kind in [LaunchKind::Qa, LaunchKind::Task] {
        assert!(
            gate_start(&task(5238, "x"), &kind, state.sessions(), false).is_none(),
            "{kind:?} was gated by a smoke test"
        );
    }
}

#[test]
fn a_second_smoke_test_is_gated_and_force_overrides_it() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![smoke_session("smoke", 5238, 1000)]);
    let gate = gate_start(
        &task(5238, "x"),
        &LaunchKind::Smoke,
        state.sessions(),
        false,
    );
    let Some(Gate::AlreadyRunning { running }) = gate else {
        panic!("a second smoke test was not gated: {gate:?}");
    };
    assert_eq!(running[0].session_id, "smoke");
    assert!(gate_start(&task(5238, "x"), &LaunchKind::Smoke, state.sessions(), true).is_none());
}

#[test]
fn a_live_qa_session_does_not_gate_a_smoke_test() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("qa", Some(5238), 1000)]);
    assert!(gate_start(
        &task(5238, "x"),
        &LaunchKind::Smoke,
        state.sessions(),
        false
    )
    .is_none());
    // The QA guard itself is unchanged.
    assert!(gate_start(&task(5238, "x"), &LaunchKind::Qa, state.sessions(), false).is_some());
}

#[test]
fn every_role_that_can_start_qa_gets_the_smoke_test_next_to_it() {
    for role in [UserRole::Qa, UserRole::Dev, UserRole::Pm] {
        let (_dir, mut state) = board_state();
        state.role = role;
        let menu = TaskMenu::build(&task(5238, "x"), &state);
        let at = |wanted: &TaskAction| menu.entries.iter().position(|(_, a)| a == wanted);
        let qa = at(&TaskAction::Start(LaunchKind::Qa)).expect("QA entry");
        let dry = at(&TaskAction::Start(LaunchKind::QaDry)).expect("QA dry run entry");
        let smoke = at(&TaskAction::Start(LaunchKind::Smoke))
            .unwrap_or_else(|| panic!("{role:?} has no smoke test entry"));
        assert!(
            qa < smoke && dry < smoke,
            "{role:?}: smoke is not next to QA"
        );
        assert!(menu.entries[smoke].0.contains("Smoke test"));
    }
}

#[test]
fn a_smoke_launch_runs_the_skill_with_no_token_no_stage_move_and_its_own_window() {
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Smoke),
    );
    let Some(Action::Launch(spec)) = actions(&mut state)
        .into_iter()
        .find(|a| matches!(a, Action::Launch(_)))
    else {
        panic!("no launch");
    };
    let prompt = spec
        .prompt
        .clone()
        .expect("a smoke launch carries a prompt");
    assert!(prompt.contains("Run /smoke-test 5238"), "{prompt}");
    assert!(!prompt.contains("/qa "), "{prompt}");
    assert!(
        !prompt.contains(crate::term::REVIEWER_TOKEN_ENV),
        "{prompt}"
    );
    assert_eq!(spec.title, "smoke-5238");
    assert_eq!(spec.reviewer_token, None);
    assert_eq!(spec.stage_move, None);
    assert_eq!(spec.flags, "");
    assert_eq!(spec.launch_kind.as_deref(), Some("smoke"));
}

#[test]
fn a_smoke_launch_exports_its_task_and_its_kind() {
    let harness = super::launch::harness();
    let repo = tempfile::tempdir().expect("repo");
    let spec = crate::ui::board::LaunchSpec {
        task_id: 5238,
        cwd: repo.path().to_string_lossy().into_owned(),
        flags: String::new(),
        prompt: Some("x".to_string()),
        title: "smoke-5238".to_string(),
        stage_move: None,
        known_session_ids: Vec::new(),
        say: String::new(),
        run_id: None,
        reviewer_token: None,
        launch_kind: Some("smoke".to_string()),
    };
    let (tx, _rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::board::launch(&spec, &harness.services, &driver, SpawnPolicy::Allow, &tx);
    let launches = harness.driver.launched();
    assert_eq!(
        launches[0].env,
        vec![
            (TASK_ID_ENV.to_string(), "5238".to_string()),
            (LAUNCH_KIND_ENV.to_string(), "smoke".to_string()),
        ]
    );
    assert_eq!(launches[0].title.as_deref(), Some("smoke-5238"));
}

#[test]
fn a_run_never_takes_a_smoke_test_for_its_qa_session() {
    let sessions = [smoke_session("smoke", 5238, 1000)];
    let rows = crate::ui::board::run_task_sessions(sessions.iter(), &[5238], |_| None);
    assert!(rows.is_empty(), "a smoke test stood in for the QA pass");
}

#[test]
fn a_smoke_test_still_shows_as_the_tasks_live_session() {
    let sessions = [smoke_session("smoke", 5238, 1000)];
    let found = crate::ui::board::task_session(sessions.iter(), 5238, None);
    assert_eq!(found.map(|s| s.session_id.as_str()), Some("smoke"));
}

#[test]
fn the_detail_pane_says_smoke_test_running() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![smoke_session("smoke", 5238, 1000)]);
    let pane = crate::ui::board::detail::state_of(&state, 5238);
    assert!(pane.running && pane.smoke);

    with_sessions(
        &mut state,
        vec![
            smoke_session("smoke", 5238, 1000),
            live_session("qa", Some(5238), 1000),
        ],
    );
    assert!(!crate::ui::board::detail::state_of(&state, 5238).smoke);
}
