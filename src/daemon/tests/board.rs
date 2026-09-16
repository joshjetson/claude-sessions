//! The board poll: warmed at start, refreshed on the slow tick, and never
//! allowed to take the sessions list down with it.
//!
//! The Node engine warmed the board in `start()` without awaiting it, because
//! `start` is what a client waits on before its first snapshot and one Odoo
//! round trip is seconds. That property is what these pin.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::daemon::{BoardFilter, EngineEvent};
use crate::types::{Board, BoardProject, BoardStage, Task};

use super::{engine_with, Setup};

/// A board fetch that counts its calls and answers with whatever is scripted.
struct Fetcher {
    calls: AtomicUsize,
    filters: Mutex<Vec<BoardFilter>>,
    answer: Mutex<Result<Board, String>>,
}

fn board_with(task_id: i64) -> Board {
    let mut board = Board {
        task_count: 1,
        ..Board::default()
    };
    board.projects.insert(
        "Repo".to_string(),
        BoardProject {
            project_id: 3,
            stages: [(
                "In Progress".to_string(),
                BoardStage {
                    stage_id: 2,
                    sequence: 2,
                    tasks: vec![Task {
                        id: task_id,
                        name: "Report templates".into(),
                        stage_id: 2,
                        stage_name: "In Progress".into(),
                        project_id: 3,
                        project_name: "Repo".into(),
                        ..Task::default()
                    }],
                },
            )]
            .into_iter()
            .collect(),
        },
    );
    board
}

fn setup(answer: Result<Board, String>) -> (Setup, Arc<Fetcher>) {
    let fetcher = Arc::new(Fetcher {
        calls: AtomicUsize::new(0),
        filters: Mutex::new(Vec::new()),
        answer: Mutex::new(answer),
    });
    let handle = Arc::clone(&fetcher);
    let board: crate::daemon::BoardFetch = Box::new(move |filter| {
        handle.calls.fetch_add(1, Ordering::SeqCst);
        handle.filters.lock().unwrap().push(filter);
        handle.answer.lock().unwrap().clone()
    });
    (
        Setup {
            board: Some(board),
            ..Setup::default()
        },
        fetcher,
    )
}

#[test]
fn the_board_is_fetched_and_indexed() {
    let (setup, fetcher) = setup(Ok(board_with(5238)));
    let harness = engine_with(setup);
    harness.engine.refresh_board();
    harness.engine.join_workers();

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    let state = harness.state();
    assert!(state.board.is_some());
    // Mandate #10: a map hit, not the nested scan `findTaskById` was.
    assert_eq!(state.task(5238).map(|task| task.id), Some(5238));
    assert!(!state.board_loading);
    assert!(state.board_error.is_none());
}

#[test]
fn a_failed_fetch_keeps_the_previous_board_and_records_why() {
    let (setup, fetcher) = setup(Ok(board_with(5238)));
    let harness = engine_with(setup);
    harness.engine.refresh_board();
    harness.engine.join_workers();

    *fetcher.answer.lock().unwrap() = Err("Odoo said no".into());
    harness.engine.refresh_board();
    harness.engine.join_workers();

    let state = harness.state();
    assert!(state.board.is_some(), "a failed fetch threw the board away");
    assert_eq!(state.board_error.as_deref(), Some("Odoo said no"));
    assert!(!state.board_loading);
}

#[test]
fn clients_are_told_the_fetch_started_and_then_how_it_went() {
    let (setup, _fetcher) = setup(Ok(board_with(5238)));
    let harness = engine_with(setup);
    let events = harness.engine.subscribe();
    harness.engine.refresh_board();
    harness.engine.join_workers();

    let seen: Vec<EngineEvent> = events.try_iter().collect();
    assert!(
        seen.iter()
            .any(|event| matches!(event, EngineEvent::Board { loading: true, .. })),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|event| matches!(
            event,
            EngineEvent::Board {
                loading: false,
                error: None
            }
        )),
        "{seen:?}"
    );
}

#[test]
fn the_filter_reaches_the_query_and_changing_it_drops_the_board() {
    let (setup, fetcher) = setup(Ok(board_with(5238)));
    let harness = engine_with(setup);
    harness.engine.refresh_board();
    harness.engine.join_workers();
    assert_eq!(
        fetcher.filters.lock().unwrap().as_slice(),
        [BoardFilter::Mine]
    );

    harness.engine.set_board_filter(BoardFilter::All);
    // The old board is the OTHER filter's answer.
    assert!(harness.state().board.is_none());
    assert!(harness.state().task(5238).is_none());

    harness.engine.refresh_board();
    harness.engine.join_workers();
    assert_eq!(
        fetcher.filters.lock().unwrap().as_slice(),
        [BoardFilter::Mine, BoardFilter::All]
    );
}

#[test]
fn an_engine_with_no_board_hook_simply_has_no_board() {
    // A daemon with no Odoo credentials must still scan, archive and notify.
    let harness = super::engine();
    harness.engine.refresh_board();
    harness.engine.join_workers();
    assert!(harness.state().board.is_none());
    assert!(!harness.state().board_loading);
}

#[test]
fn start_does_not_wait_for_the_board() {
    // `start` is what a client waits on before its first snapshot; one Odoo
    // round trip is seconds. The fetch is warmed and left to finish.
    let (setup, fetcher) = setup(Ok(board_with(5238)));
    let harness = engine_with(setup);
    harness.engine.start();
    harness.engine.stop();
    assert!(
        fetcher.calls.load(Ordering::SeqCst) >= 1,
        "the board was never warmed"
    );
}
