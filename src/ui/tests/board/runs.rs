//! Watching a stage as a QA run, from the board.
//!
//! These are the assertions about what the KEYS do. What a run row says lives
//! in `board::tests::qarun`; what a run decides lives in `qarun::tests`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::fixtures::{board_state, task, with_tasks};
use crate::board::BoardItem;
use crate::qarun::RunMode;
use crate::ui::board::{self, BoardRow};
use crate::ui::state::View;

fn press(state: &mut crate::ui::state::AppState, ch: char) {
    board::handle_board(state, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
}

fn down(state: &mut crate::ui::state::AppState) {
    board::handle_board(state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
}

/// A board with three tasks in one stage, the cursor on the stage row.
fn on_a_stage() -> (tempfile::TempDir, crate::ui::state::AppState) {
    let (dir, mut state) = board_state();
    state.view = View::Board;
    with_tasks(
        &mut state,
        vec![
            task(4101, "Summary row shows the wrong total"),
            task(4102, "Description field ignores its length cap"),
            task(4103, "Number formatting differs between panels"),
        ],
    );
    // Open the project so the stage row exists, then land on it.
    let snapshot = board::snapshot(&state);
    if let BoardRow::Project { .. } = snapshot.row {
        board::handle_board(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );
        down(&mut state);
    }
    (dir, state)
}

#[test]
fn r_on_a_stage_starts_watching_it() {
    let (_dir, mut state) = on_a_stage();
    assert!(matches!(
        board::snapshot(&state).row,
        BoardRow::Stage { .. }
    ));

    press(&mut state, 'R');

    assert_eq!(state.board.runs.len(), 1, "no run was created");
    let run = &state.board.runs[0];
    assert_eq!(run.task_ids.len(), 3);
    // Shadow is the default: it is the mode that cannot be wrong in a way a QA
    // pass would not notice.
    assert_eq!(run.mode, RunMode::Shadow);
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("3 tasks")));
}

#[test]
fn watching_the_same_stage_twice_does_not_make_two_runs() {
    // Two runs over the same tasks would each claim the rows, and the board
    // would draw them twice.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();

    // Move back onto the stage row and press again.
    state.board.stop_watching("nothing");
    let before = state.board.runs.len();
    state.board.watch_stage("Aurora", "Approved to Start");
    assert_eq!(state.board.runs.len(), before);
    assert_eq!(state.board.runs[0].id, run_id);
}

#[test]
fn a_run_keeps_a_task_the_stage_has_lost() {
    // The reason a run is a record and not a view over the stage.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    assert_eq!(state.board.runs[0].task_ids.len(), 3);

    // The stage now holds one task; the run still covers all three.
    with_tasks(
        &mut state,
        vec![task(4101, "Summary row shows the wrong total")],
    );
    state.board.watch_stage("Aurora", "Approved to Start");
    assert_eq!(
        state.board.runs[0].task_ids.len(),
        3,
        "a task that left the stage was dropped from the run"
    );
}

#[test]
fn a_run_puts_its_rows_on_the_board() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    assert!(
        snapshot.keys.iter().any(|key| key.starts_with("br:")),
        "no run header row: {:?}",
        snapshot.keys
    );
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|key| key.starts_with("brt:"))
            .count(),
        3
    );
    // …and its tasks are no longer loose rows underneath it.
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|key| key.starts_with("bt:"))
            .count(),
        0,
        "run tasks also appeared as loose stage rows"
    );
}

#[test]
fn r_on_a_run_row_stops_watching() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    assert_eq!(state.board.runs.len(), 1);

    // Land on the run header and press again.
    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    press(&mut state, 'R');

    assert!(state.board.runs.is_empty(), "the run was not dropped");
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("untouched")));
}

#[test]
fn r_on_a_row_that_is_neither_says_so() {
    let (_dir, mut state) = board_state();
    press(&mut state, 'R');
    assert!(state.board.runs.is_empty());
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("Select a stage")));
}

#[test]
fn the_jump_key_says_so_when_nothing_is_waiting() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    press(&mut state, ']');
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("No agent is waiting")));
}

#[test]
fn enter_on_a_run_header_opens_the_run_menu() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    assert!(
        matches!(state.dialog, Some(crate::ui::dialogs::Dialog::RunMenu(_))),
        "the run menu did not open"
    );
}

#[test]
fn collapsing_a_run_hides_its_rows_without_releasing_its_tasks() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    let after = board::snapshot(&state);
    assert_eq!(
        after.keys.iter().filter(|k| k.starts_with("brt:")).count(),
        0,
        "collapsing left the run's rows on screen"
    );
    assert_eq!(
        after.keys.iter().filter(|k| k.starts_with("bt:")).count(),
        0,
        "collapsing released the run's tasks back to the stage"
    );
}

#[test]
fn a_run_row_is_still_a_task_row() {
    // Every key that works on a board task works here: that is most of the
    // argument for putting runs on the board rather than in their own tab.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let first = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("brt:"))
        .expect("a run task row");
    state.board_sel.set(&snapshot.keys, first);

    let row = board::snapshot(&state).row;
    assert!(matches!(row, BoardRow::QaRunTask { task: Some(_), .. }));
    assert!(row.task().is_some(), "a run row did not answer as a task");
}

#[test]
fn the_board_is_unchanged_when_nothing_is_watched() {
    let (_dir, mut state) = on_a_stage();
    // Open the stage by hand: watching one opens it as a side effect, and this
    // test is about the board WITHOUT a run.
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );

    let snapshot = board::snapshot(&state);
    assert!(
        !snapshot.keys.iter().any(|key| key.starts_with("br")),
        "a run row appeared with no run: {:?}",
        snapshot.keys
    );
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|k| k.starts_with("bt:"))
            .count(),
        3,
        "the ordinary task rows changed: {:?}",
        snapshot.keys
    );
    let _ = BoardItem::Separator;
}

#[test]
fn the_run_menu_offers_a_context_prompt() {
    // The JS original lets you type something for the coordinator before it
    // starts. This is that entry.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    let Some(crate::ui::dialogs::Dialog::RunMenu(menu)) = &state.dialog else {
        panic!("the run menu did not open");
    };
    assert!(
        menu.entries.iter().any(
            |(_, action)| *action == crate::ui::dialogs::RunAction::StartCoordinatorWithContext
        ),
        "the run menu has no context entry: {:?}",
        menu.entries
            .iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    );
}

#[test]
fn typed_context_reaches_the_coordinator_launch() {
    // The context prompt hands back a StartCoordinator carrying the text, so
    // the launch path needs no second entry point.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();

    let mut prompt = crate::ui::dialogs::RunContext::new(&run_id, "Approved to Start");
    let mut config = state.config.clone();
    let mut ctx = crate::ui::dialogs::DialogCtx {
        config: &mut config,
    };
    for ch in "preview is down".chars() {
        prompt.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut ctx,
        );
    }
    // Multiline, so Enter is a newline and Ctrl-S submits — the same contract
    // the task context dialog uses.
    let outcome = prompt.handle_key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        &mut ctx,
    );

    match outcome {
        crate::ui::dialogs::DialogOutcome::Run(command) => {
            assert_eq!(command.run_id, run_id);
            assert_eq!(
                command.action,
                crate::ui::dialogs::RunAction::StartCoordinator
            );
            assert_eq!(command.context, "preview is down");
        }
        other => panic!("expected a run command, got {other:?}"),
    }
}
