//! A database that will not open must never take the tool with it.
//!
//! The files are still written beside the rows, so the filesystem is always the
//! fallback — but only if every accessor answers neutrally instead of throwing.
//! This is the posture the Node app got from wrapping every statement in
//! `try {} catch {}`; here it is the one wrapper in [`Db::with_conn`], so these
//! tests exercise the whole public surface against a broken store.

use super::*;
use std::fs;

/// A database file full of something that is not a database.
fn corrupt() -> (TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).unwrap();
    fs::write(
        &paths.db_path,
        b"this is not a SQLite file, it is a ransom note",
    )
    .unwrap();
    let db = Db::open(&paths);
    (dir, db)
}

/// A path that cannot become a file at all.
fn unopenable() -> (TempDir, Paths, Db) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.db_path).unwrap();
    let db = Db::open(&paths);
    (dir, paths, db)
}

#[test]
fn a_corrupt_file_is_reported_not_fatal() {
    let (_dir, db) = corrupt();
    assert!(!db.available());
    assert!(
        db.first_error().is_some(),
        "the failure must be recorded for the daemon to log once"
    );
    assert_eq!(db.schema_version(), 0);
}

#[test]
fn a_path_that_cannot_be_a_database_is_reported_not_fatal() {
    let (_dir, _paths, db) = unopenable();
    assert!(!db.available());
    assert!(db.first_error().is_some());
}

#[test]
fn every_reader_returns_the_neutral_value() {
    let (_dir, db) = corrupt();

    assert_eq!(db.get_task_archive(5944), None);
    assert!(db.list_archived_task_ids().is_empty());
    assert_eq!(db.task_session(5944), None);
    assert_eq!(db.task_for_session_file("/x/a.jsonl"), None);
    assert!(db.daily_log_entries(None).is_empty());
    assert!(db.daily_log_entries(Some("2026-08-01")).is_empty());
    assert!(db.task_history(5944).is_empty());
    assert!(db.list_log_days().is_empty());
    assert!(db.recent_notifications(RECENT_LIMIT).is_empty());
    assert!(!db.was_alerted("assigned:5944:qa"));
}

#[test]
fn every_writer_is_a_silent_no_op() {
    let (_dir, db) = corrupt();

    // None of these may panic, and none may leave the store looking usable.
    db.put_task_archive(&archive(5944, "sess-a"));
    db.put_task_session(&TaskSession {
        task_id: 5944,
        session_file: "/x/sess-a.jsonl".to_string(),
        cwd: "/repo".to_string(),
        updated_at: "2026-08-01T10:00:00.000Z".to_string(),
    });
    db.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T10:00:00.000Z",
        100,
        "One",
    ));
    db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    db.set_notification_status(&["n1"], NotificationStatus::Read);
    db.mark_notifications_read(&["n1"]);
    db.mark_notifications_read::<&str>(&[]);
    db.delete_notifications(&["n1"]);
    db.prune_notifications(PRUNE_KEEP);
    db.mark_alerted("assigned:5944:qa");

    assert_eq!(db.get_task_archive(5944), None);
    assert!(db.recent_notifications(10).is_empty());
    assert!(!db.was_alerted("assigned:5944:qa"));
}

#[test]
fn the_import_reports_nothing_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).unwrap();
    fs::write(&paths.db_path, b"not a database").unwrap();
    fs::create_dir_all(paths.tasks_dir.join("5944")).unwrap();
    fs::write(
        paths.tasks_dir.join("5944").join("meta.json"),
        r#"{"taskId":5944}"#,
    )
    .unwrap();

    let db = Db::open(&paths);
    assert_eq!(db.import_existing_files(&paths), ImportReport::default());
}

#[test]
fn a_statement_failure_is_recorded_once_and_the_store_keeps_working() {
    // Open a healthy database, then make one accessor fail. The store must not
    // latch shut, and the record must be the FIRST failure, not the last.
    let t = open();
    t.db.exec("t", |conn| conn.execute_batch("DROP TABLE task_archive"));

    assert_eq!(t.db.get_task_archive(5944), None);
    let first = t.db.first_error().unwrap().to_string();
    assert!(first.starts_with("get_task_archive"), "unexpected: {first}");

    assert!(t.db.list_archived_task_ids().is_empty());
    assert_eq!(t.db.first_error().unwrap(), first, "only the first is kept");

    // Everything else still works.
    t.db.put_notification(&notification(
        "n1",
        "2026-08-01T10:00:00.000Z",
        "still here",
    ));
    assert_eq!(t.db.recent_notifications(10).len(), 1);
    assert!(t.db.available());
}
