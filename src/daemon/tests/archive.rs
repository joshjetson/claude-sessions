//! Ported from `test/auto-archive.test.js` — archiving the transcript of a task
//! session that goes away.
//!
//! This used to need one of two things: the agent running the done hook, or the
//! in-memory link still holding the session. The first is a prompt instruction
//! a project can override away — two projects had — and the second is daemon
//! memory, empty after a restart. Eight sessions ended with no archive at all,
//! including a whole epic and two merge-conflict sessions, whose prompt never
//! mentions the done hook by design.

use std::time::Duration;

use super::*;
use crate::daemon::{TaskLink, TaskLinkStatus};

#[test]
fn with_no_in_memory_link_at_all_the_post_restart_case() {
    let mut harness = engine();
    let session = harness.transcript(880001, "/repo/a");
    let session_id = session.session_id.clone();

    harness.auto_archive(vec![session]); // tick 1: session present
    assert_eq!(
        harness.engine.db().get_task_archive(880001),
        None,
        "archived while still running"
    );

    harness.auto_archive(vec![]); // tick 2: session gone
    let row = harness
        .engine
        .db()
        .get_task_archive(880001)
        .expect("a session that ended left no archive");
    assert_eq!(row.session_id, session_id);
    assert!(harness.state().archived_tasks.contains(&880001));
}

#[test]
fn even_though_the_links_were_empty_throughout() {
    let mut harness = engine();
    let session = harness.transcript(880002, "/repo/a");
    assert!(harness.state().task_sessions.is_empty(), "precondition");
    harness.auto_archive(vec![session]);
    harness.auto_archive(vec![]);
    assert!(harness.engine.db().get_task_archive(880002).is_some());
}

#[test]
fn a_still_running_session_is_left_alone() {
    let mut harness = engine();
    let session = harness.transcript(880003, "/repo/a");
    harness.auto_archive(vec![session.clone()]);
    harness.auto_archive(vec![session]);
    assert_eq!(
        harness.engine.db().get_task_archive(880003),
        None,
        "archived a session that is still open"
    );
}

#[test]
fn a_session_with_no_task_reference_is_ignored() {
    // Hand-started sessions carry no task URL; they are not task work.
    let mut harness = engine();
    harness.auto_archive(vec![a_session("plain", "/repo/a")]);
    harness.auto_archive(vec![]);
    assert!(harness.state().seen_task_sessions.is_empty());
}

#[test]
fn an_already_archived_task_is_not_re_archived() {
    let mut harness = engine();
    let session = harness.transcript(880004, "/repo/a");
    harness.auto_archive(vec![session.clone()]);
    harness.auto_archive(vec![]);
    let first = harness
        .engine
        .db()
        .get_task_archive(880004)
        .unwrap()
        .archived_at;

    harness.auto_archive(vec![session]);
    harness.auto_archive(vec![]);
    assert_eq!(
        harness
            .engine
            .db()
            .get_task_archive(880004)
            .unwrap()
            .archived_at,
        first,
        "archived twice"
    );
}

#[test]
fn several_sessions_ending_at_once_are_all_archived() {
    // A whole epic ended this way: a batch of sessions, none archived.
    let mut harness = engine();
    let batch: Vec<Session> = [880010, 880011, 880012]
        .iter()
        .map(|id| harness.transcript(*id, &format!("/repo/{id}")))
        .collect();
    harness.auto_archive(batch);
    harness.auto_archive(vec![]);
    for id in [880010, 880011, 880012] {
        assert!(
            harness.engine.db().get_task_archive(id).is_some(),
            "#{id} was not archived"
        );
    }
}

#[test]
fn the_link_based_path_still_works_for_a_transcript_with_no_task_in_it() {
    // Belt and braces: a dashboard-spawned session whose prompt lacks the URL.
    let mut harness = engine();
    let session = harness.transcript(880005, "/repo/a");
    harness.state().task_sessions.insert(
        880005,
        TaskLink {
            cwd: session.cwd.clone(),
            session_id: session.session_id.clone(),
            session_file: session.session_file.clone(),
            status: Some(TaskLinkStatus::Running),
            ..TaskLink::default()
        },
    );
    harness.auto_archive(vec![Session {
        task_id: None,
        ..session
    }]);
    harness.auto_archive(vec![]);
    assert!(
        harness.engine.db().get_task_archive(880005).is_some(),
        "the link-based fallback stopped working"
    );
    assert_eq!(
        harness.state().task_sessions[&880005].status,
        Some(TaskLinkStatus::Ended)
    );
}

// --- every QA round is archived, not just the first -------------------------

#[test]
fn round_2_replaces_round_1_in_the_archive() {
    // Round 1 ends and is archived. Round 2 then runs in the same folder and
    // ends; the archive must move to round 2. Skipping on "this task already
    // has an archive" froze one task's archive on a 167-line ready-check stub
    // while its real QA session held 2963 lines.
    let mut harness = engine();
    let round1 = harness.aged_transcript(970101, "/repo/b", Duration::from_secs(60));
    harness.auto_archive(vec![round1.clone()]);
    harness.auto_archive(vec![]);
    assert_eq!(
        archived_session_id(&harness, 970101).as_deref(),
        Some(round1.session_id.as_str()),
        "round 1 was not archived"
    );

    let round2 = harness.transcript(970101, "/repo/b");
    harness.auto_archive(vec![round2.clone()]);
    harness.auto_archive(vec![]);
    assert_eq!(
        archived_session_id(&harness, 970101).as_deref(),
        Some(round2.session_id.as_str()),
        "the archive stayed on round 1"
    );
}

#[test]
fn the_same_session_ending_twice_is_not_re_archived() {
    let mut harness = engine();
    let only = harness.transcript(970102, "/repo/b");
    harness.auto_archive(vec![only.clone()]);
    harness.auto_archive(vec![]);
    let first = harness
        .engine
        .db()
        .get_task_archive(970102)
        .unwrap()
        .archived_at;

    harness.auto_archive(vec![only]);
    harness.auto_archive(vec![]);
    assert_eq!(
        harness
            .engine
            .db()
            .get_task_archive(970102)
            .unwrap()
            .archived_at,
        first,
        "re-archived the same session"
    );
}

#[test]
fn the_archived_transcript_is_on_disk_where_resume_will_look() {
    let mut harness = engine();
    let session = harness.transcript(970103, "/repo/b");
    harness.auto_archive(vec![session.clone()]);
    harness.auto_archive(vec![]);
    let copy = harness
        .paths
        .task_dir(970103)
        .join(format!("{}.jsonl", session.session_id));
    assert!(exists(&copy), "{} was not written", copy.display());
    assert!(exists(&harness.paths.task_dir(970103).join("meta.json")));
}

// --- the 4b contract --------------------------------------------------------

#[test]
fn a_live_task_session_is_indexed_every_time_it_moves() {
    // The QA flow moves a task's work to a folder it was never launched in. The
    // index is the only thing that finds it afterwards without reading every
    // transcript on the disk.
    let mut harness = engine();
    let first = harness.transcript(970104, "/repo/b");
    harness.auto_archive(vec![first.clone()]);
    assert_eq!(
        harness.engine.db().task_session(970104).map(|row| row.cwd),
        Some("/repo/b".to_string())
    );

    let moved = harness.transcript(970104, "/Users/nobody/Desktop/QAden/task-970104-qa");
    harness.auto_archive(vec![moved.clone()]);
    let row = harness.engine.db().task_session(970104).unwrap();
    assert_eq!(row.cwd, "/Users/nobody/Desktop/QAden/task-970104-qa");
    assert_eq!(
        row.session_file,
        moved.session_file.unwrap().to_string_lossy()
    );
}
