//! Opening and migrating.

use super::*;

/// The store has to be usable from several daemon threads.
#[test]
fn the_store_is_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Db>();
}

#[test]
fn opens_and_migrates_on_first_use() {
    let t = open();
    assert!(t.db.available());
    assert_eq!(t.db.schema_version(), MIGRATIONS.len() as i64);
    assert!(t.db.first_error().is_none());
}

#[test]
fn the_connection_matches_the_node_store() {
    let t = open();
    let journal: String =
        t.db.one("t", "PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
    let foreign_keys: i64 =
        t.db.one("t", "PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
    let busy: i64 =
        t.db.one("t", "PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy, 3000);
}

#[test]
fn migrating_an_already_migrated_database_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());

    let first = Db::open(&paths);
    first.put_task_archive(&archive(5944, "sess-a"));
    first.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T10:00:00.000Z",
        100,
        "One",
    ));
    let version = first.schema_version();
    drop(first);

    // Re-opening runs migrate() again against a file that has seen every
    // migration. Nothing may be re-applied and nothing may be lost.
    let second = Db::open(&paths);
    assert_eq!(second.schema_version(), version);
    assert_eq!(second.get_task_archive(5944), Some(archive(5944, "sess-a")));
    assert_eq!(second.daily_log_entries(None).len(), 1);
    drop(second);

    let third = Db::open(&paths);
    assert_eq!(third.schema_version(), version);
    assert_eq!(third.daily_log_entries(None).len(), 1);
    assert_eq!(third.list_archived_task_ids(), vec![5944]);
}

#[test]
fn a_database_written_by_a_newer_version_is_not_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    let db = Db::open(&paths);
    db.exec("t", |conn| conn.execute_batch("PRAGMA user_version = 99"));
    drop(db);

    let reopened = Db::open(&paths);
    assert!(reopened.available());
    assert_eq!(reopened.schema_version(), 99);
}

/// A database left at the Node app's last version gains the new index table and
/// keeps every row it already held.
#[test]
fn a_node_era_database_migrates_forward_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());

    let db = Db::open(&paths);
    db.put_task_archive(&archive(6117, "sess-node"));
    db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "from node"));
    // Rewind to where the Node app left off: the schema is identical for
    // migrations 1 and 2, so this is what an existing file looks like.
    //
    // Every later migration has to be undone here, not just the newest. An
    // earlier version of this test dropped only migration 3's table and left
    // migration 4's column in place, so reopening re-ran `ADD COLUMN kind`
    // against a column that already existed and the whole open failed. The
    // rewind has to be a real version 2 or it is testing a state that cannot
    // occur.
    db.exec("t", |conn| {
        conn.execute_batch(
            // The index has to go before the column it indexes: SQLite refuses
            // DROP COLUMN while an index references it, and Db::exec swallows
            // the error — so a wrong order here leaves the file at version 4
            // with migration 3's table already dropped, and the test fails
            // somewhere else entirely.
            "DROP INDEX IF EXISTS notifications_kind_idx;
             ALTER TABLE notifications DROP COLUMN kind;
             DROP TABLE task_session_index;
             PRAGMA user_version = 2",
        )
    });
    drop(db);

    let migrated = Db::open(&paths);
    assert_eq!(migrated.schema_version(), MIGRATIONS.len() as i64);
    assert_eq!(
        migrated.get_task_archive(6117).unwrap().session_id,
        "sess-node"
    );
    assert_eq!(migrated.recent_notifications(10).len(), 1);
    // …and the new table is there and usable.
    migrated.put_task_session(&TaskSession {
        task_id: 6117,
        session_file: "/x/sess-node.jsonl".to_string(),
        cwd: "/repo/portal".to_string(),
        updated_at: "2026-08-02T10:00:00.000Z".to_string(),
    });
    assert!(migrated.task_session(6117).is_some());
}

/// The migration list is shared with the Node app, which writes the same
/// database file. `PRAGMA user_version` is one integer for both, so a migration
/// that exists here at index N and means something else there at index N is a
/// silent corruption: whichever app opens the file first sets the version and
/// the other skips its own without running it and without erroring.
///
/// These assertions exist so that renumbering fails here rather than in
/// someone's notification feed six weeks later.
#[test]
fn the_shared_migration_sequence_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    let db = Db::open(&paths);

    assert_eq!(
        db.schema_version(),
        4,
        "the schema version moved — is the Node app's src/db.ts at the same number?"
    );

    // 3 — this port's task/transcript index. The Node app carries the same SQL
    // at the same index without using the table, purely to keep the sequences
    // aligned.
    assert!(
        table_exists(&db, "task_session_index"),
        "migration 3 is missing — the sequence has diverged from the Node app"
    );

    // 4 — the notification kind.
    assert!(
        notification_columns(&db).contains(&"kind".to_string()),
        "migration 4 is missing — the sequence has diverged from the Node app"
    );
}

/// The end-to-end shape of the failure the pinning guards: a missing column
/// means the INSERT throws, `Db::exec` swallows it, and notifications stop
/// persisting without a word anywhere.
#[test]
fn a_notification_round_trips_its_kind() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    let db = Db::open(&paths);

    let mut asking = notification("q1", "2026-09-17T12:00:00.000Z", "which environment?");
    asking.kind = crate::types::NotificationKind::Question;
    db.put_notification(&asking);

    let stored = db.recent_notifications(10);
    assert_eq!(stored.len(), 1, "the notification was not persisted at all");
    assert_eq!(stored[0].kind, crate::types::NotificationKind::Question);
}

/// A row written before the column existed, and any sender that omits it, reads
/// back as the kind those notifications always were.
#[test]
fn an_unknown_kind_reads_as_info() {
    assert_eq!(
        crate::types::NotificationKind::from_label("nonsense"),
        crate::types::NotificationKind::Info
    );
    assert_eq!(
        crate::types::NotificationKind::from_label(""),
        crate::types::NotificationKind::Info
    );
    assert!(!crate::types::NotificationKind::Info.is_answerable());
    assert!(!crate::types::NotificationKind::Verdict.is_answerable());
    assert!(crate::types::NotificationKind::Question.is_answerable());
}

fn table_exists(db: &Db, name: &str) -> bool {
    db.one(
        "t",
        "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |row| row.get::<_, String>(0),
    )
    .is_some()
}

fn notification_columns(db: &Db) -> Vec<String> {
    db.rows("cols", "PRAGMA table_info(notifications)", [], |row| {
        row.get::<_, String>(1)
    })
}
