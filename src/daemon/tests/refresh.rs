//! The tick itself: what it produces, what it costs a second time, and what it
//! survives.

use std::fs::OpenOptions;
use std::io::Write;
use std::thread;
use std::time::Duration;

use super::*;
use crate::daemon::{EngineEvent, RefreshRequest};

fn open_cursors(harness: &TestEngine) -> usize {
    harness.inner().scan().caches.open_cursors()
}

fn bytes_read(harness: &TestEngine) -> u64 {
    harness.inner().scan().caches.bytes_read()
}

#[test]
fn a_tick_pairs_enriches_groups_and_announces() {
    let mut harness = engine();
    let session = harness.transcript(6137, "/repo/app");
    harness.procs.add(501, "/repo/app").launched_for(501, 6137);
    let events = harness.engine.subscribe();

    harness.engine.refresh(RefreshRequest::default());

    {
        let state = harness.state();
        assert_eq!(state.sessions.len(), 1, "the session was not discovered");
        let found = state
            .sessions
            .get(&session.session_id)
            .expect("indexed by its own session id");
        assert_eq!(found.task_id, Some(6137), "task read from the transcript");
        assert_eq!(found.cwd, "/repo/app");
        assert_eq!(found.pids, vec![501]);
        assert_eq!(
            state.sessions.by_project().keys().collect::<Vec<_>>(),
            vec!["repo/app"]
        );
    }

    let announced = events
        .try_iter()
        .filter_map(|event| match event {
            EngineEvent::Sessions(payload) => Some(*payload),
            _ => None,
        })
        .last()
        .expect("no sessions event");
    assert_eq!(announced.stats.total_sessions, 1);
    assert_eq!(announced.stats.total_projects, 1);
}

#[test]
fn a_process_with_no_transcript_yet_is_a_starting_placeholder() {
    let harness = engine();
    harness.procs.add(502, "/repo/empty");
    harness.engine.refresh(RefreshRequest::default());
    let state = harness.state();
    let session = state
        .sessions
        .iter()
        .next()
        .expect("no row for the process");
    assert!(session.starting);
    // Node recomputed the status here and turned every placeholder into "idle".
    assert_eq!(session.status, SessionStatus::Starting);
    assert!(session.session_file.is_none());
}

#[test]
fn one_cursor_per_live_session_and_none_for_a_vanished_one() {
    let mut harness = engine();
    harness.transcript(6137, "/repo/app");
    harness.procs.add(501, "/repo/app").launched_for(501, 6137);

    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(open_cursors(&harness), 1);
    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(open_cursors(&harness), 1, "a second cursor was opened");
    assert_eq!(
        harness.inner().scan().caches.opened(),
        1,
        "the cold-start path ran twice"
    );

    harness.procs.remove(501);
    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(
        open_cursors(&harness),
        0,
        "the cursor outlived its session, holding the file open"
    );
}

#[test]
fn a_second_tick_reads_only_what_was_appended() {
    // Mandate #1. Node re-read the last 512 KB of every live transcript once a
    // second; here a tick costs the size of the append.
    let mut harness = engine();
    let session = harness.transcript(6137, "/repo/app");
    let path = session.session_file.clone().unwrap();
    harness.procs.add(501, "/repo/app").launched_for(501, 6137);

    harness.engine.refresh(RefreshRequest::default());
    let after_cold_start = bytes_read(&harness);
    assert!(after_cold_start > 0);

    let line = serde_json::json!({
        "type": "assistant",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] },
    })
    .to_string();
    let appended = line.len() as u64 + 1;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{line}\n").as_bytes())
        .unwrap();

    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(
        bytes_read(&harness) - after_cold_start,
        appended,
        "the tick re-read more than the append"
    );
    assert_eq!(
        harness
            .state()
            .sessions
            .iter()
            .next()
            .unwrap()
            .activity_detail,
        "responding",
        "the appended entry was not folded in"
    );
}

#[test]
fn configured_groups_are_discovered_and_carried_into_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("repo-one")).unwrap();
    std::fs::create_dir_all(dir.path().join("repo-two")).unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
    let harness = engine_with(Setup {
        config: Some(serde_json::json!({
            "groups": [{ "name": "work", "path": format!("{}/", dir.path().display()) }]
        })),
        ..Setup::default()
    });

    harness.engine.refresh(RefreshRequest::discovery());
    let snapshot = harness.engine.snapshot();
    let dirs = snapshot
        .sessions
        .discovered_dirs
        .get(dir.path().to_string_lossy().as_ref())
        .expect("the group was not discovered");
    assert_eq!(dirs, &vec!["repo-one".to_string(), "repo-two".to_string()]);
}

// --- re-entrancy ------------------------------------------------------------

#[test]
fn concurrent_refreshes_collapse_into_one_trailing_run() {
    let harness = engine();
    harness.procs.set_delay(Duration::from_millis(150));

    thread::scope(|scope| {
        scope.spawn(|| harness.engine.refresh(RefreshRequest::default()));
        // Wait until the first tick is provably inside the scan (the fake
        // counts on entry, then sleeps its delay) — a fixed sleep here lost
        // the race on slow CI runners and the pile-on ran uncollapsed.
        while harness.procs.listed() == 0 {
            thread::yield_now();
        }
        for _ in 0..3 {
            harness.engine.refresh(RefreshRequest::default());
        }
    });

    let stats = harness.engine.stats();
    assert_eq!(stats.collapsed, 3, "a concurrent call was not collapsed");
    assert_eq!(
        stats.refreshes, 2,
        "three collapsed calls must produce exactly one trailing run"
    );
    // …and the scan really only ran twice.
    assert_eq!(harness.procs.listed(), 2);
}

#[test]
fn a_collapsed_call_asking_for_discovery_makes_the_trailing_run_force_it() {
    let harness = engine();
    harness.procs.set_delay(Duration::from_millis(120));
    thread::scope(|scope| {
        scope.spawn(|| harness.engine.refresh(RefreshRequest::default()));
        thread::sleep(Duration::from_millis(40));
        harness.engine.refresh(RefreshRequest::discovery());
    });
    assert_eq!(harness.engine.stats().refreshes, 2);
}

#[test]
fn a_tick_that_panics_does_not_kill_the_loop() {
    let mut harness = engine();
    harness.transcript(6137, "/repo/app");
    harness.procs.add(501, "/repo/app").launched_for(501, 6137);

    harness.procs.set_explode(true);
    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(harness.engine.stats().panics, 1);
    assert!(harness.state().sessions.is_empty());

    // The next tick retries, against a scanner whose mutex the panic poisoned.
    harness.procs.set_explode(false);
    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(harness.engine.stats().panics, 1);
    assert_eq!(
        harness.state().sessions.len(),
        1,
        "the loop never recovered"
    );
}

// --- lifecycle --------------------------------------------------------------

#[test]
fn start_seeds_from_the_store_and_stop_joins_the_loop() {
    let mut harness = engine();
    let session = harness.transcript(6137, "/repo/app");
    harness.procs.add(501, "/repo/app").launched_for(501, 6137);
    // An archive from a previous run, and a notification that arrived while the
    // dashboard was closed.
    harness.auto_archive(vec![session]);
    harness.auto_archive(vec![]);
    harness
        .engine
        .db()
        .put_notification(&crate::types::Notification {
            id: "old-1".to_string(),
            title: "while you were out".to_string(),
            message: String::new(),
            cwd: String::new(),
            project: String::new(),
            session_id: None,
            task_id: None,
            level: crate::types::NotificationLevel::Info,
            kind: crate::types::NotificationKind::Info,
            run_id: String::new(),
            ts: "2026-09-16T10:00:00.000Z".to_string(),
            status: crate::types::NotificationStatus::Unread,
        });
    // A marker left by a previous run must never be replayed.
    std::fs::create_dir_all(&harness.paths.done_dir).unwrap();
    std::fs::write(harness.paths.done_marker(4242), "{\"taskId\":4242}").unwrap();

    harness.engine.start();
    let started = harness.engine.stats().refreshes;
    assert!(started >= 1, "start() did not refresh synchronously");
    harness.engine.stop();

    let state = harness.state();
    assert!(
        state.archived_tasks.contains(&6137),
        "archived set not seeded"
    );
    assert_eq!(state.notifications.len(), 1, "feed not restored");
    assert!(
        !state.done_tasks.contains(&4242),
        "a stale marker was replayed"
    );
    assert!(!harness.paths.done_marker(4242).exists());
}

#[test]
fn starting_twice_is_a_no_op_and_stopping_twice_is_safe() {
    let harness = engine();
    harness.engine.start();
    harness.engine.start();
    harness.engine.stop();
    harness.engine.stop();
}

#[test]
fn the_loop_thread_keeps_ticking_and_stops_when_told() {
    let harness = engine();
    // A launch in flight puts the loop on its fast cadence, so this waits half
    // a second rather than a whole one.
    harness.engine.set_pending(crate::daemon::PendingRequest {
        cwd: "/repo/app".to_string(),
        task_id: Some(1),
        known_session_ids: Vec::new(),
    });

    harness.engine.start();
    let after_start = harness.engine.stats().refreshes;
    thread::sleep(Duration::from_millis(700));
    let while_running = harness.engine.stats().refreshes;
    harness.engine.stop();
    thread::sleep(Duration::from_millis(700));
    let after_stop = harness.engine.stats().refreshes;

    assert!(
        while_running > after_start,
        "the loop thread never ticked ({after_start} -> {while_running})"
    );
    assert_eq!(
        after_stop, while_running,
        "the loop kept running after stop()"
    );
}

#[test]
fn a_fresh_board_is_indexed_and_announced() {
    let harness = engine();
    let events = harness.engine.subscribe();
    harness.engine.set_board(None, None);
    assert!(harness.engine.snapshot().board_loading);

    let mut board = crate::types::Board::default();
    board.projects.insert(
        "Project A".to_string(),
        crate::types::BoardProject {
            project_id: 11,
            stages: std::collections::BTreeMap::from([(
                "QA".to_string(),
                crate::types::BoardStage {
                    stage_id: 4,
                    sequence: 2,
                    tasks: vec![a_task(55, "QA")],
                },
            )]),
        },
    );
    harness.engine.set_board(Some(board), None);

    assert!(!harness.engine.snapshot().board_loading);
    assert_eq!(harness.state().task(55).map(|task| task.id), Some(55));
    let announced: Vec<bool> = events
        .try_iter()
        .filter_map(|event| match event {
            EngineEvent::Board { loading, .. } => Some(loading),
            _ => None,
        })
        .collect();
    assert_eq!(announced, vec![true, false]);
}

// --- whether an empty tick can be believed -----------------------------------

fn last_sessions_event(
    events: &std::sync::mpsc::Receiver<EngineEvent>,
) -> crate::daemon::SessionsEvent {
    events
        .try_iter()
        .filter_map(|event| match event {
            EngineEvent::Sessions(payload) => Some(*payload),
            _ => None,
        })
        .last()
        .expect("no sessions event")
}

#[test]
fn a_tick_after_the_last_session_is_killed_says_its_empty_list_is_the_truth() {
    let harness = engine();
    harness.procs.add_bystander(900);
    harness.procs.add(501, "/repo/app");
    let events = harness.engine.subscribe();
    harness.engine.refresh(RefreshRequest::default());
    let first = last_sessions_event(&events);
    assert_eq!(first.stats.total_sessions, 1);
    assert!(first.scan_complete);

    harness.procs.remove(501);
    harness.engine.refresh(RefreshRequest::default());

    let announced = last_sessions_event(&events);
    assert!(announced.by_project.is_empty());
    assert!(
        announced.scan_complete,
        "a readable listing with no sessions was announced as a failed read"
    );
    assert!(harness.state().sessions_complete);
}

#[test]
fn a_tick_that_read_an_empty_listing_does_not_vouch_for_it() {
    let harness = engine();
    let events = harness.engine.subscribe();
    harness.engine.refresh(RefreshRequest::default());

    let announced = last_sessions_event(&events);
    assert!(announced.by_project.is_empty());
    assert!(!announced.scan_complete);
}
