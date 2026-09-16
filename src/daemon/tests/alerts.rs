//! Ported from `test/alerts.test.js` — the two alerts the daemon raises
//! without you watching the board, and the first-run rule that keeps the
//! backlog quiet.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::daemon::{
    detect_new_assignments, detect_stalls, human_duration, normalise_stage, StallOptions, TaskLink,
    TaskLinkStatus,
};

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

const NOW: u64 = 1_700_000_000;

fn running(session_id: &str) -> TaskLink {
    TaskLink {
        cwd: "/repo".to_string(),
        session_id: session_id.to_string(),
        status: Some(TaskLinkStatus::Running),
        ..TaskLink::default()
    }
}

fn with_status(session_id: &str, status: TaskLinkStatus) -> TaskLink {
    TaskLink {
        status: Some(status),
        ..running(session_id)
    }
}

fn links(pairs: impl IntoIterator<Item = (i64, TaskLink)>) -> BTreeMap<i64, TaskLink> {
    pairs.into_iter().collect()
}

/// A session whose transcript last moved `ago_min` minutes before `NOW`.
fn sess(session_id: &str, ago_min: u64, status: SessionStatus) -> Session {
    Session {
        session_mtime: at(NOW - ago_min * 60),
        status,
        ..a_session(session_id, "/repo")
    }
}

fn options(stuck_min: u64, remind_min: u64) -> StallOptions {
    StallOptions {
        now: at(NOW),
        stuck_after: Duration::from_secs(stuck_min * 60),
        remind_every: Duration::from_secs(remind_min * 60),
    }
}

fn stalls(
    links: &BTreeMap<i64, TaskLink>,
    sessions: &[Session],
    options: &StallOptions,
    last_alerted: &HashMap<i64, SystemTime>,
) -> Vec<crate::daemon::Stall> {
    // The lookup is a closure, so a caller can hand in a slice, a map or an
    // index — the Node version took "a Map or a plain object" for the same
    // reason.
    detect_stalls(
        links,
        |session_id| sessions.iter().find(|s| s.session_id == session_id),
        options,
        last_alerted,
    )
}

// --- new assignments --------------------------------------------------------

fn never(_key: &str) -> bool {
    false
}

fn watched() -> Vec<String> {
    vec!["Approved to Start".to_string()]
}

#[test]
fn flags_a_task_in_a_watched_stage() {
    let tasks = [a_task(1, "Approved to Start")];
    let found = detect_new_assignments(&tasks, &watched(), never);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task.id, 1);
    assert_eq!(found[0].key, "assigned:1:approved to start");
}

#[test]
fn ignores_tasks_in_other_stages() {
    let tasks = [
        a_task(1, "Backlog"),
        a_task(2, "In Progress"),
        a_task(3, "QA"),
    ];
    assert!(detect_new_assignments(&tasks, &watched(), never).is_empty());
}

#[test]
fn matches_stage_names_regardless_of_case_and_padding() {
    let tasks = [a_task(1, "  approved TO start ")];
    assert_eq!(detect_new_assignments(&tasks, &watched(), never).len(), 1);
}

#[test]
fn does_not_announce_the_same_task_twice() {
    let tasks = [a_task(1, "Approved to Start")];
    let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let once = |key: &str| seen.lock().unwrap().iter().any(|k| k == key);

    let first = detect_new_assignments(&tasks, &watched(), once);
    assert_eq!(first.len(), 1);
    let key = first[0].key.clone();
    drop(first);
    seen.lock().unwrap().push(key);

    let second = detect_new_assignments(&tasks, &watched(), once);
    assert!(second.is_empty(), "the task was announced a second time");
}

#[test]
fn a_task_that_moves_on_and_comes_back_is_announced_again() {
    // Keyed by stage: coming back to Approved to Start after a revision is
    // genuinely new work to pick up.
    let seen = ["assigned:1:approved to start".to_string()];
    let once = |key: &str| seen.iter().any(|k| k == key);

    let waiting = [a_task(1, "Approved to Start")];
    assert!(detect_new_assignments(&waiting, &watched(), once).is_empty());

    let in_progress = [a_task(1, "In Progress")];
    let moved = detect_new_assignments(&in_progress, &["In Progress".to_string()], once);
    assert_eq!(moved.len(), 1, "a different stage must alert separately");
}

#[test]
fn multiple_watched_stages_are_supported() {
    let tasks = [
        a_task(1, "Approved to Start"),
        a_task(2, "Revision Required"),
        a_task(3, "Backlog"),
    ];
    let stages = vec![
        "Approved to Start".to_string(),
        "Revision Required".to_string(),
    ];
    let found = detect_new_assignments(&tasks, &stages, never);
    assert_eq!(
        found.iter().map(|f| f.task.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn malformed_rows_are_skipped_not_fatal() {
    let tasks = [
        Task::default(),
        a_task(0, "Approved to Start"),
        Task {
            stage_name: "Approved to Start".to_string(),
            ..Task::default()
        },
    ];
    assert!(detect_new_assignments(&tasks, &watched(), never).is_empty());
    assert!(detect_new_assignments(&[], &watched(), never).is_empty());
}

// --- stalled sessions -------------------------------------------------------

#[test]
fn flags_a_session_silent_past_the_threshold() {
    let found = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 20, SessionStatus::Idle)],
        &options(15, 30),
        &HashMap::new(),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task_id, 5);
    assert_eq!(found[0].silent, 20 * MINUTE);
}

#[test]
fn leaves_a_session_that_is_still_writing_alone() {
    let found = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 2, SessionStatus::Working)],
        &options(15, 30),
        &HashMap::new(),
    );
    assert!(found.is_empty());
}

#[test]
fn only_watches_sessions_believed_to_be_running() {
    let links = links([
        (1, with_status("a", TaskLinkStatus::Done)),
        (2, with_status("b", TaskLinkStatus::Blocked)),
        (3, with_status("c", TaskLinkStatus::Ended)),
        (4, running("d")),
    ]);
    let sessions: Vec<Session> = ["a", "b", "c", "d"]
        .iter()
        .map(|id| sess(id, 30, SessionStatus::Idle))
        .collect();
    let found = stalls(&links, &sessions, &options(15, 30), &HashMap::new());
    assert_eq!(found.iter().map(|s| s.task_id).collect::<Vec<_>>(), vec![4]);
}

#[test]
fn a_vanished_session_is_not_reported_as_stalled() {
    // It has ended; auto-archiving handles that path separately.
    let found = stalls(
        &links([(5, running("gone"))]),
        &[],
        &options(15, 30),
        &HashMap::new(),
    );
    assert!(found.is_empty());
}

#[test]
fn does_not_repeat_inside_the_reminder_window() {
    let last = HashMap::from([(5i64, at(NOW - 5 * 60))]);
    let found = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 40, SessionStatus::Idle)],
        &options(15, 30),
        &last,
    );
    assert!(found.is_empty());
}

#[test]
fn repeats_once_the_reminder_window_has_passed() {
    let last = HashMap::from([(5i64, at(NOW - 45 * 60))]);
    let found = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 60, SessionStatus::Idle)],
        &options(15, 30),
        &last,
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn remind_every_zero_means_alert_once_and_never_again() {
    let last = HashMap::from([(5i64, at(NOW - 600 * 60))]);
    let found = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 600, SessionStatus::Idle)],
        &options(15, 0),
        &last,
    );
    assert!(found.is_empty());
}

#[test]
fn surfaces_whether_claude_appears_to_be_waiting_on_you() {
    let waiting = stalls(
        &links([(5, running("a"))]),
        &[sess("a", 20, SessionStatus::Awaiting)],
        &options(15, 30),
        &HashMap::new(),
    );
    assert!(waiting[0].awaiting);
    let quiet = stalls(
        &links([(6, running("b"))]),
        &[sess("b", 20, SessionStatus::Idle)],
        &options(15, 30),
        &HashMap::new(),
    );
    assert!(!quiet[0].awaiting);
}

#[test]
fn a_session_with_no_mtime_is_skipped_rather_than_infinitely_stalled() {
    let found = stalls(
        &links([(5, running("a"))]),
        &[no_mtime(a_session("a", "/repo"))],
        &options(15, 30),
        &HashMap::new(),
    );
    assert!(found.is_empty());
}

#[test]
fn the_session_lookup_can_be_a_map_as_well_as_a_slice() {
    let sessions: HashMap<String, Session> =
        HashMap::from([("a".to_string(), sess("a", 20, SessionStatus::Idle))]);
    let found = detect_stalls(
        &links([(5, running("a"))]),
        |session_id| sessions.get(session_id),
        &options(15, 30),
        &HashMap::new(),
    );
    assert_eq!(found.len(), 1);
}

// --- formatting -------------------------------------------------------------

#[test]
fn human_duration_minutes_singular_and_plural() {
    assert_eq!(human_duration(MINUTE), "1 minute");
    assert_eq!(human_duration(18 * MINUTE), "18 minutes");
}

#[test]
fn human_duration_hours_with_and_without_trailing_minutes() {
    assert_eq!(human_duration(60 * MINUTE), "1 hour");
    assert_eq!(human_duration(65 * MINUTE), "1 hour 5 minutes");
    assert_eq!(human_duration(120 * MINUTE), "2 hours");
}

#[test]
fn human_duration_rounds_down_rather_than_up() {
    assert_eq!(human_duration(Duration::from_secs(119)), "1 minute");
}

#[test]
fn normalise_stage_trims_and_lowercases() {
    assert_eq!(normalise_stage("  Approved To Start "), "approved to start");
    assert_eq!(normalise_stage(""), "");
}
