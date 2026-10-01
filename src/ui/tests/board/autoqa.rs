//! Auto QA, dashboard side: an arrival joins its run, gets a coordinator, and
//! starts, with the run's own admission deciding.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::fixtures::{actions, board_state, live_session, task, with_sessions, with_tasks};
use crate::daemon::{AutoArrival, AutoQaFeed};
use crate::types::Task;
use crate::ui::board::{self, apply_auto_qa};
use crate::ui::state::{Action, AppState};

const PROJECT: &str = "Aurora";
const STAGE: &str = "QA";

/// A QA task in the Auto QA project. Not on the board: the reviewer's board
/// shows their own tasks, and this one is assigned to nobody.
fn qa_task(id: i64) -> Task {
    Task {
        project_name: PROJECT.to_string(),
        stage_name: STAGE.to_string(),
        ..task(id, "Arrived in QA")
    }
}

fn arrival(id: i64) -> AutoArrival {
    AutoArrival {
        key: format!("autoqa:{id}:qa:2026-09-30"),
        task_id: id,
        project: PROJECT.to_string(),
        stage: STAGE.to_string(),
        entered: "2026-09-30 10:00:00".to_string(),
    }
}

/// A board state with Auto QA on for the project and one repo folder mapped,
/// so a start goes all the way to a launch.
fn auto_state() -> (tempfile::TempDir, AppState) {
    let (dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir(PROJECT, "/tmp/aurora")
        .unwrap();
    state.config.set_auto_qa(PROJECT, true).unwrap();
    (dir, state)
}

/// The launches queued, as (task id, whether it is a coordinator).
fn launches(queued: &[Action]) -> Vec<(i64, bool)> {
    queued
        .iter()
        .filter_map(|action| match action {
            Action::Launch(spec) => Some((spec.task_id, spec.run_id.is_some())),
            _ => None,
        })
        .collect()
}

fn run_id() -> String {
    crate::qarun::QaRun::id_for(PROJECT, STAGE)
}

#[test]
fn an_arrival_joins_its_run_gets_a_coordinator_and_starts() {
    let (_dir, mut state) = auto_state();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![arrival(7001)],
            ..AutoQaFeed::default()
        },
    );

    let run = state
        .board
        .runs
        .iter()
        .find(|run| run.id == run_id())
        .expect("the stage's run");
    assert_eq!(run.task_ids, vec![7001]);
    assert!(run.coordinator_started);
    assert_eq!(run.spawned, vec![7001]);

    let queued = actions(&mut state);
    assert_eq!(launches(&queued), vec![(7001, true), (7001, false)]);
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::AutoQaJoined(keys) if keys == &vec![arrival(7001).key]
        )),
        "the daemon was not told the arrival joined"
    );
}

/// No notification: a session starting is not news.
#[test]
fn an_arrival_raises_no_notification() {
    let (_dir, mut state) = auto_state();
    let before = state.notifications.len();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![arrival(7001)],
            ..AutoQaFeed::default()
        },
    );
    assert_eq!(state.notifications.len(), before);
    assert!(!actions(&mut state)
        .iter()
        .any(|action| matches!(action, Action::Sound(_))));
}

/// A second arrival while the coordinator is running joins the same run, and
/// the coordinator is told rather than a second one launched.
#[test]
fn a_running_coordinator_is_told_about_a_task_that_joined() {
    let (_dir, mut state) = auto_state();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![arrival(7001)],
            ..AutoQaFeed::default()
        },
    );
    let mut coordinator = live_session("coord", None, 1000);
    coordinator.run_id = Some(run_id());
    with_sessions(&mut state, vec![coordinator]);
    actions(&mut state);

    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001), qa_task(7002)],
            arrivals: vec![arrival(7002)],
            ..AutoQaFeed::default()
        },
    );
    let queued = actions(&mut state);
    assert!(!launches(&queued)
        .iter()
        .any(|(_, coordinator)| *coordinator));
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::NudgeCoordinator(spec) if spec.text.contains("#7002")
        )),
        "the coordinator was not told"
    );
    assert_eq!(state.board.runs[0].task_ids, vec![7001, 7002]);
}

/// Back in QA after a revision: a new round, so the run may start it again.
#[test]
fn a_task_back_in_qa_can_start_again() {
    let (_dir, mut state) = auto_state();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![arrival(7001)],
            ..AutoQaFeed::default()
        },
    );
    actions(&mut state);
    state.auto_coordinator_at.clear();

    let mut back = arrival(7001);
    back.key = "autoqa:7001:qa:2026-10-01".to_string();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![back],
            ..AutoQaFeed::default()
        },
    );
    assert!(launches(&actions(&mut state)).contains(&(7001, false)));
    assert_eq!(state.board.runs[0].task_ids, vec![7001]);
}

/// The run's own admission still decides. A task Odoo sent back joins the run
/// and says why it does not start.
#[test]
fn a_task_odoo_sent_back_joins_but_does_not_start() {
    let (_dir, mut state) = auto_state();
    let mut sent_back = qa_task(7003);
    sent_back.state = Some(crate::odoo::task_state::CHANGES_REQUESTED.to_string());
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![sent_back],
            arrivals: vec![arrival(7003)],
            ..AutoQaFeed::default()
        },
    );
    assert!(!launches(&actions(&mut state)).contains(&(7003, false)));
    let sections = state.run_sections();
    let reason = sections[0].agents[0]
        .idle_reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("Changes Requested"), "{reason}");
}

/// Switched off on this machine: the arrival is confirmed, so it is not handed
/// over again, and nothing joins or starts.
#[test]
fn a_project_switched_off_confirms_and_starts_nothing() {
    let (_dir, mut state) = auto_state();
    state.config.set_auto_qa(PROJECT, false).unwrap();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            arrivals: vec![arrival(7001)],
            ..AutoQaFeed::default()
        },
    );
    let queued = actions(&mut state);
    assert!(launches(&queued).is_empty());
    assert!(state.board.runs.is_empty());
    assert!(queued
        .iter()
        .any(|action| matches!(action, Action::AutoQaJoined(keys) if keys.len() == 1)));
}

// --- the A key ----------------------------------------------------------------

fn on_the_project_row(state: &mut AppState) {
    with_tasks(state, vec![task(4101, "x")]);
    let snapshot = board::snapshot(state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| *key == crate::board::project_key("NoSuchProject-ForTests"))
        .expect("a project row");
    state.board_sel.set(&snapshot.keys, index);
}

fn press_a(state: &mut AppState) {
    board::handle_board(state, KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
}

#[test]
fn a_switches_auto_qa_on_and_off_for_a_project_with_one_folder() {
    let (_dir, mut state) = board_state();
    on_the_project_row(&mut state);
    let project = task(4101, "x").project_name;
    state
        .config
        .add_odoo_project_dir(&project, "/tmp/repo")
        .unwrap();

    press_a(&mut state);
    assert!(state.config.qa_auto(&project));
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("Auto QA is on")));

    press_a(&mut state);
    assert!(!state.config.qa_auto(&project));
}

/// An Auto QA start has nobody at the keyboard, so a project with no folder,
/// or several, would stop at a picker. Switching on refuses and says why.
#[test]
fn a_refuses_a_project_without_exactly_one_folder() {
    let (_dir, mut state) = board_state();
    on_the_project_row(&mut state);
    let project = task(4101, "x").project_name;

    press_a(&mut state);
    assert!(!state.config.qa_auto(&project));
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("exactly one repo folder")));
}

/// The daemon hands an arrival over until it hears back, so a feed can carry
/// one twice. It joins once and the coordinator hears once, but the daemon is
/// told again, in case the first word was lost.
#[test]
fn an_arrival_seen_twice_joins_once() {
    let (_dir, mut state) = auto_state();
    let feed = || AutoQaFeed {
        tasks: vec![qa_task(7001)],
        arrivals: vec![arrival(7001)],
        ..AutoQaFeed::default()
    };
    apply_auto_qa(&mut state, feed());
    let mut coordinator = live_session("coord", None, 1000);
    coordinator.run_id = Some(run_id());
    with_sessions(&mut state, vec![coordinator]);
    actions(&mut state);

    apply_auto_qa(&mut state, feed());
    let queued = actions(&mut state);
    assert!(launches(&queued).is_empty(), "started twice");
    assert!(!queued
        .iter()
        .any(|action| matches!(action, Action::NudgeCoordinator(_))));
    assert!(queued.iter().any(|action| matches!(
        action,
        Action::AutoQaJoined(keys) if keys == &vec![arrival(7001).key]
    )));
}

/// Put tasks on the board the way a fetch does, and return what that queued.
fn show_on_board(state: &mut AppState, tasks: Vec<Task>) -> Vec<Action> {
    use crate::types::{Board, BoardProject, BoardStage};
    let mut board = Board {
        task_count: tasks.len(),
        ..Board::default()
    };
    board.projects.insert(
        PROJECT.to_string(),
        BoardProject {
            project_id: 3,
            stages: [(
                STAGE.to_string(),
                BoardStage {
                    stage_id: 1,
                    sequence: 1,
                    tasks,
                },
            )]
            .into_iter()
            .collect(),
        },
    );
    state.apply_board(crate::ui::board::BoardUpdate::loaded(
        crate::daemon::BoardFilter::Mine,
        board,
    ));
    actions(state)
}

fn asked(queued: &[Action]) -> bool {
    queued
        .iter()
        .any(|action| matches!(action, Action::AutoQaCheck))
}

/// If the board can show a task in QA, the daemon is asked to look now rather
/// than at its next read. Once per task, and never for one the feed has.
#[test]
fn a_task_the_board_shows_in_qa_first_asks_for_a_check() {
    let (_dir, mut state) = auto_state();
    assert!(asked(&show_on_board(&mut state, vec![qa_task(7001)])));
    assert!(
        !asked(&show_on_board(&mut state, vec![qa_task(7001)])),
        "asked again for the same task"
    );

    // The feed has 7002 already, so the daemon has seen it.
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7002)],
            ..AutoQaFeed::default()
        },
    );
    assert!(!asked(&show_on_board(
        &mut state,
        vec![qa_task(7001), qa_task(7002)]
    )));

    // 7001 leaves QA and comes back: that asks again.
    show_on_board(&mut state, Vec::new());
    assert!(asked(&show_on_board(&mut state, vec![qa_task(7001)])));
}

#[test]
fn with_auto_qa_off_the_board_asks_nothing() {
    let (_dir, mut state) = board_state();
    assert!(!asked(&show_on_board(&mut state, vec![qa_task(7001)])));
}

#[test]
fn the_status_says_what_auto_qa_is_doing() {
    let (_dir, mut state) = auto_state();
    let now = std::time::SystemTime::now();
    assert_eq!(
        board::auto_qa_badge(&state, now).as_deref(),
        Some("Auto QA: no daemon feed")
    );

    let checked: chrono::DateTime<chrono::Utc> = (now - std::time::Duration::from_secs(8)).into();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            watching: vec![PROJECT.to_string()],
            checked_at: checked.to_rfc3339(),
            ..AutoQaFeed::default()
        },
    );
    assert_eq!(
        board::auto_qa_badge(&state, now).as_deref(),
        Some("Auto QA ✓ 8s ago")
    );
    assert_eq!(
        board::auto_qa_status_line(&state, now).as_deref(),
        Some("Watching Aurora · 1 task in QA · checked 8s ago")
    );

    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            watching: vec![PROJECT.to_string()],
            checked_at: checked.to_rfc3339(),
            error: Some("No Odoo credentials.".to_string()),
            ..AutoQaFeed::default()
        },
    );
    assert_eq!(
        board::auto_qa_badge(&state, now).as_deref(),
        Some("Auto QA: check failed")
    );
    assert!(board::auto_qa_status_line(&state, now)
        .unwrap()
        .ends_with("The last check failed: No Odoo credentials."));

    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            paused: vec![PROJECT.to_string()],
            checked_at: checked.to_rfc3339(),
            ..AutoQaFeed::default()
        },
    );
    assert_eq!(
        board::auto_qa_badge(&state, now).as_deref(),
        Some("Auto QA: paused")
    );
    assert_eq!(
        board::auto_qa_status_line(&state, now).as_deref(),
        Some("Another machine keeps Aurora · checked 8s ago")
    );
}

#[test]
fn with_auto_qa_off_there_is_no_status() {
    let (_dir, state) = board_state();
    assert!(board::auto_qa_badge(&state, std::time::SystemTime::now()).is_none());
    assert!(board::auto_qa_status_line(&state, std::time::SystemTime::now()).is_none());
}

/// End to end through the real draw: the board's title carries the badge, and
/// the settings page carries the full line under its Auto QA section.
#[test]
fn the_board_and_the_settings_page_show_the_status() {
    let (_dir, mut state) = auto_state();
    let checked: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    apply_auto_qa(
        &mut state,
        AutoQaFeed {
            tasks: vec![qa_task(7001)],
            watching: vec![PROJECT.to_string()],
            checked_at: checked.to_rfc3339(),
            ..AutoQaFeed::default()
        },
    );
    with_tasks(&mut state, vec![qa_task(7001)]);
    let painted = crate::ui::tests::text(&crate::ui::tests::render(140, 30, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(painted.contains("Auto QA ✓"), "{painted}");

    state.dialog = Some(crate::ui::dialogs::Dialog::Settings(
        crate::ui::dialogs::SettingsDialog {
            page: crate::ui::dialogs::settings::Page::Qa,
            ..Default::default()
        },
    ));
    let painted = crate::ui::tests::text(&crate::ui::tests::render(140, 40, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(
        painted.contains("Watching Aurora · 1 task in QA · checked"),
        "{painted}"
    );
}
