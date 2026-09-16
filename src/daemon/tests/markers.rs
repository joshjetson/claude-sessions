//! The `done/` and `blocked/` directories: cleaned at startup, polled on the
//! tick, and never replayed.

use std::fs;
use std::time::{Duration, SystemTime};

use super::*;
use crate::daemon::DoneMarker;

/// Backdate a marker so the settle window has passed.
fn settle(path: &std::path::Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(1))
        .unwrap();
}

fn write_done(harness: &TestEngine, task_id: i64, summary: &str) -> std::path::PathBuf {
    fs::create_dir_all(&harness.paths.done_dir).unwrap();
    let path = harness.paths.done_marker(task_id);
    fs::write(
        &path,
        serde_json::json!({
            "taskId": task_id,
            "cwd": "/repo/app",
            "summary": summary,
            "ts": crate::util::iso_now(),
        })
        .to_string(),
    )
    .unwrap();
    path
}

#[test]
fn startup_creates_both_directories_and_clears_what_is_in_them() {
    let harness = engine();
    fs::create_dir_all(&harness.paths.done_dir).unwrap();
    let stale = write_done(&harness, 4242, "from a previous run");
    fs::write(harness.paths.done_dir.join("notes.txt"), "keep me").unwrap();

    harness.inner().clean_stale_markers();

    assert!(harness.paths.done_dir.is_dir());
    assert!(harness.paths.blocked_dir.is_dir());
    assert!(
        !stale.exists(),
        "an old completion would have been replayed"
    );
    assert!(
        harness.paths.done_dir.join("notes.txt").exists(),
        "removed a file that is not a marker"
    );
}

#[test]
fn a_marker_still_being_written_is_left_for_the_next_poll() {
    let harness = engine();
    let path = write_done(&harness, 4243, "half written");
    harness.inner().poll_markers();
    harness.inner().join_workers();
    assert!(path.exists(), "read a marker inside the settle window");
    assert!(harness.state().done_tasks.is_empty());
}

#[test]
fn a_settled_done_marker_is_read_removed_and_acted_on() {
    let mut harness = engine();
    harness.transcript(4244, "/repo/app");
    let path = write_done(&harness, 4244, "**Fix:** the guard");
    settle(&path);

    harness.inner().poll_markers();
    harness.inner().join_workers();

    assert!(!path.exists(), "the marker was not consumed");
    let state = harness.state();
    assert!(state.done_tasks.contains(&4244));
    assert!(state.notifications[0].title.contains("#4244"));
    assert!(harness
        .backend
        .comment()
        .is_some_and(|html| html.contains("<b>Fix:</b>")));
}

#[test]
fn a_settled_blocked_marker_records_its_questions() {
    let harness = engine();
    fs::create_dir_all(&harness.paths.blocked_dir).unwrap();
    let path = harness.paths.blocked_marker(4245);
    fs::write(
        &path,
        serde_json::json!({
            "taskId": 4245,
            "cwd": "/repo/app",
            "questions": ["Which environment?"],
        })
        .to_string(),
    )
    .unwrap();
    settle(&path);

    harness.inner().poll_markers();
    harness.inner().join_workers();

    assert!(!path.exists());
    assert_eq!(
        harness.state().blocked_tasks[&4245].questions,
        vec!["Which environment?".to_string()]
    );
}

#[test]
fn a_malformed_marker_is_dropped_rather_than_retried_forever() {
    // Under `fs.watch` an unparseable marker was simply never looked at again;
    // under polling it would be re-read on every tick for as long as the daemon
    // ran.
    let harness = engine();
    fs::create_dir_all(&harness.paths.done_dir).unwrap();
    let path = harness.paths.done_dir.join("9999.json");
    fs::write(&path, "{ this is not json").unwrap();
    settle(&path);

    harness.inner().poll_markers();
    harness.inner().join_workers();

    assert!(!path.exists());
    assert!(harness.state().notifications.is_empty());
}

#[test]
fn a_marker_summary_is_capped_however_it_was_written() {
    let harness = engine();
    let huge = "x".repeat(9000);
    let path = write_done(&harness, 4246, &huge);
    settle(&path);

    harness.inner().poll_markers();
    harness.inner().join_workers();

    let comment = harness.backend.comment().expect("no comment");
    assert!(
        comment.len() < 9000,
        "an oversized summary reached Odoo whole"
    );
}

#[test]
fn a_completion_posted_over_the_action_api_runs_off_the_caller_thread() {
    // The shape Phase 6's `POST /done` takes: answer 202, do the work behind it.
    let harness = engine();
    harness.engine.process_done(DoneMarker {
        task_id: 4247,
        cwd: "/repo/app".to_string(),
        summary: "signed off".to_string(),
        ts: String::new(),
    });
    harness.engine.join_workers();
    assert!(harness.state().done_tasks.contains(&4247));
}
