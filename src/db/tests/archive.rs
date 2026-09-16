//! The task archive, and the task-to-session index that replaces the Node app's
//! scan of every project directory.

use super::*;

#[test]
fn round_trips_a_record() {
    let t = open();
    let row = archive(5944, "sess-a");
    t.db.put_task_archive(&row);
    assert_eq!(t.db.get_task_archive(5944), Some(row));
}

#[test]
fn re_archiving_the_same_task_updates_rather_than_duplicating() {
    let t = open();
    t.db.put_task_archive(&archive(5944, "sess-a"));

    let mut round_two = archive(5944, "sess-b");
    round_two.archived_at = "2026-07-24T10:00:00.000Z".to_string();
    t.db.put_task_archive(&round_two);

    let stored = t.db.get_task_archive(5944).unwrap();
    assert_eq!(stored.session_id, "sess-b");
    assert_eq!(stored.archived_at, "2026-07-24T10:00:00.000Z");
    assert_eq!(
        t.db.list_archived_task_ids()
            .iter()
            .filter(|id| **id == 5944)
            .count(),
        1,
        "round 2 must replace round 1, not sit beside it"
    );
}

#[test]
fn an_unknown_task_returns_none_not_an_error() {
    let t = open();
    assert_eq!(t.db.get_task_archive(999_999), None);
}

#[test]
fn lists_every_archived_id() {
    let t = open();
    t.db.put_task_archive(&archive(6117, "sess-c"));
    t.db.put_task_archive(&archive(5944, "sess-a"));
    assert_eq!(t.db.list_archived_task_ids(), vec![5944, 6117]);
}

fn session(task_id: i64, file: &str, updated_at: &str) -> TaskSession {
    TaskSession {
        task_id,
        session_file: file.to_string(),
        cwd: "/repo/portal".to_string(),
        updated_at: updated_at.to_string(),
    }
}

#[test]
fn the_task_session_index_round_trips() {
    let t = open();
    let row = session(
        6688,
        "/proj/-repo-portal/abc.jsonl",
        "2026-08-01T10:00:00.000Z",
    );
    t.db.put_task_session(&row);
    assert_eq!(t.db.task_session(6688), Some(row));
    assert_eq!(t.db.task_session(4033), None);
}

#[test]
fn a_task_that_moves_keeps_one_index_row() {
    let t = open();
    t.db.put_task_session(&session(
        6688,
        "/proj/a/round1.jsonl",
        "2026-08-01T10:00:00.000Z",
    ));
    // QA relocates the work to a different folder; the newest location wins.
    let moved = session(6688, "/proj/qa/round3.jsonl", "2026-08-03T10:00:00.000Z");
    t.db.put_task_session(&moved);

    assert_eq!(t.db.task_session(6688), Some(moved));
    assert!(
        t.db.task_for_session_file("/proj/a/round1.jsonl").is_none(),
        "the stale location must not still resolve to the task"
    );
}

#[test]
fn the_index_answers_which_task_owns_a_transcript() {
    let t = open();
    t.db.put_task_session(&session(
        4033,
        "/proj/a/one.jsonl",
        "2026-08-01T10:00:00.000Z",
    ));
    t.db.put_task_session(&session(
        6440,
        "/proj/a/two.jsonl",
        "2026-08-01T11:00:00.000Z",
    ));

    // The cross-link this guards: a pending launch must not claim a transcript
    // another task already holds.
    assert_eq!(
        t.db.task_for_session_file("/proj/a/two.jsonl")
            .map(|r| r.task_id),
        Some(6440)
    );
    assert_eq!(t.db.task_for_session_file("/proj/a/three.jsonl"), None);
    assert_eq!(t.db.task_for_session_file(""), None);
}
