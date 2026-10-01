//! The QA queue and the machine-health gate, from the dashboard.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::json;

use super::fixtures::{actions, board_state, task, with_tasks};
use crate::config::{ConfigHandle, EnvOverrides};
use crate::health::{HealthLevel, Vitals};
use crate::ui::board;
use crate::ui::state::{Action, AppState};

/// A watched run of three tasks, its coordinator started, in a project mapped
/// to one folder so a start reaches a launch, with the gate as given.
fn run_of_three(gate: bool) -> (tempfile::TempDir, AppState, String) {
    let (dir, mut state) = board_state();
    std::fs::write(
        &state.paths.config_path,
        json!({
            "odooProjectDirs": { "NoSuchProject-ForTests": "/tmp/repo" },
            "qa": { "healthGate": gate },
        })
        .to_string(),
    )
    .unwrap();
    state.config = ConfigHandle::load(&state.paths, EnvOverrides::default());
    with_tasks(
        &mut state,
        vec![task(4101, "a"), task(4102, "b"), task(4103, "c")],
    );
    let (project, stage) = {
        let task = state.board.task(4101).expect("task").clone();
        (task.project_name, task.stage_name)
    };
    state.board.watch_stage(&project, &stage);
    state.board.runs[0].coordinator_started = true;
    let run_id = state.board.runs[0].id.clone();
    (dir, state, run_id)
}

fn reading(level: HealthLevel, reasons: &[&str], at: SystemTime) -> Vitals {
    Vitals {
        level,
        reasons: reasons.iter().map(|r| r.to_string()).collect(),
        sampled_at: DateTime::<Utc>::from(at).to_rfc3339(),
        ..Vitals::default()
    }
}

fn start_run(state: &mut AppState, run_id: &str) -> usize {
    board::run_command(
        state,
        crate::ui::dialogs::RunCommand {
            run_id: run_id.to_string(),
            action: crate::ui::dialogs::RunAction::StartRun,
            context: String::new(),
        },
    );
    actions(state)
        .iter()
        .filter(|action| matches!(action, Action::Launch(_)))
        .count()
}

fn first_reason(state: &AppState) -> Option<String> {
    state.run_sections()[0].agents[0].idle_reason.clone()
}

#[test]
fn a_red_machine_holds_the_queue_and_each_row_says_why() {
    let (_dir, mut state, run_id) = run_of_three(true);
    state.health = Some(reading(
        HealthLevel::Red,
        &["memory 8% free", "load 2.4 per core"],
        SystemTime::now(),
    ));
    assert_eq!(
        first_reason(&state).as_deref(),
        Some("Waiting: the machine is busy (memory 8% free, load 2.4 per core).")
    );
    assert_eq!(start_run(&mut state, &run_id), 0);
}

/// One start per reading, so the next reading includes that session's load.
#[test]
fn a_healthy_machine_starts_one_session_per_reading() {
    let (_dir, mut state, run_id) = run_of_three(true);
    state.health = Some(reading(
        HealthLevel::Amber,
        &["swap 2.9 GB used, 27% free"],
        SystemTime::now() - Duration::from_secs(5),
    ));
    assert_eq!(start_run(&mut state, &run_id), 1, "more than one start");

    // Nothing more until a reading taken after that start arrives.
    assert_eq!(start_run(&mut state, &run_id), 0);
    let waiting = state.run_sections()[0]
        .agents
        .iter()
        .find(|agent| agent.task_id == 4102)
        .and_then(|agent| agent.idle_reason.clone())
        .unwrap_or_default();
    assert!(waiting.contains("fresh machine reading"), "{waiting}");

    state.health = Some(reading(
        HealthLevel::Green,
        &[],
        SystemTime::now() + Duration::from_secs(1),
    ));
    assert_eq!(start_run(&mut state, &run_id), 1);
}

#[test]
fn with_the_gate_off_the_machine_is_not_consulted() {
    let (_dir, mut state, run_id) = run_of_three(false);
    state.health = Some(reading(
        HealthLevel::Red,
        &["memory 8% free"],
        SystemTime::now(),
    ));
    assert_eq!(start_run(&mut state, &run_id), 3);
}

/// A reading that stopped arriving holds nothing: QA is never stopped by
/// data that is missing.
#[test]
fn an_old_reading_or_none_holds_nothing() {
    let (_dir, mut state, run_id) = run_of_three(true);
    state.health = Some(reading(
        HealthLevel::Red,
        &["memory 8% free"],
        SystemTime::now() - Duration::from_secs(600),
    ));
    assert_eq!(start_run(&mut state, &run_id), 1);

    let (_dir, mut state, run_id) = run_of_three(true);
    state.health = None;
    assert_eq!(start_run(&mut state, &run_id), 1);
}
