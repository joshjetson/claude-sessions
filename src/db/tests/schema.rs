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
            // Every column added since version 2 comes off, newest first. A
            // column left behind makes its own migration fail on re-run —
            // ADD COLUMN on one that already exists is an error, `Db::exec`
            // swallows it, and the file never opens at all (version 0).
            "ALTER TABLE task_session_index DROP COLUMN reviewer_token;
             ALTER TABLE notifications DROP COLUMN run_id;
             DROP TABLE IF EXISTS qa_runs;
             DROP INDEX IF EXISTS notifications_kind_idx;
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
        7,
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

    // 5 — watched QA runs. THIS ONE IS NOT IN THE NODE APP.
    //
    // The sequences diverge here on purpose, and the reasoning is worth keeping
    // because the general rule says not to. Node is frozen: the work moved to
    // this port and nothing new is being added there. Index 5 existing here and
    // nowhere there is safe in both orders — Node's loop ends at 4 and has
    // nothing at 5 to skip, and this app applies 5 over a file Node left at 4.
    //
    // What would NOT be safe is the Node app later adding a DIFFERENT migration
    // at index 5. If that ever happens, it must be this same SQL.
    assert!(
        table_exists(&db, "qa_runs"),
        "migration 5 is missing — a watched run will not survive a restart"
    );

    // 6 — who raised a notification, when it was a coordinator. Also not in the
    // Node app, for the reason given above migration 5.
    assert!(
        notification_columns(&db).contains(&"run_id".to_string()),
        "migration 6 is missing — a coordinator will wake itself with its own escalation"
    );

    // 7 — the reviewer's token. Also not in the Node app, same reasoning.
    assert!(
        task_session_columns(&db).contains(&"reviewer_token".to_string()),
        "migration 7 is missing — the dashboard cannot sign an instruction"
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

fn task_session_columns(db: &Db) -> Vec<String> {
    db.rows("cols", "PRAGMA table_info(task_session_index)", [], |row| {
        row.get::<_, String>(1)
    })
}

fn notification_columns(db: &Db) -> Vec<String> {
    db.rows("cols", "PRAGMA table_info(notifications)", [], |row| {
        row.get::<_, String>(1)
    })
}

// --- QA runs survive a restart ------------------------------------------------

fn a_run(id: &str) -> crate::qarun::QaRun {
    crate::qarun::QaRun {
        id: id.to_string(),
        project_name: "x/alpha".to_string(),
        stage_name: "Quality Assurance".to_string(),
        task_ids: vec![4101, 4102, 4103],
        spawned: vec![4101],
        started_at: "2026-09-18T10:00:00Z".to_string(),
        lane_limit: None,
        mode: crate::qarun::RunMode::Triage,
        coordinator_started: true,
    }
}

#[test]
fn a_saved_run_comes_back_whole() {
    // The point of the table: quitting the dashboard used to turn a seven-agent
    // run back into seven unrelated rows.
    let t = open();
    let run = a_run("x/alpha::Quality Assurance");
    t.db.save_qa_runs(std::slice::from_ref(&run));

    let back = t.db.qa_runs();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0], run);
}

#[test]
fn saving_replaces_rather_than_accumulates() {
    // Stopping a run has to REMOVE its row. An upsert would leave it behind and
    // the run would come back after being dismissed.
    let t = open();
    t.db.save_qa_runs(&[a_run("one"), a_run("two")]);
    t.db.save_qa_runs(&[a_run("two")]);

    let back = t.db.qa_runs();
    assert_eq!(back.len(), 1, "the removed run came back: {back:?}");
    assert_eq!(back[0].id, "two");
}

#[test]
fn saving_nothing_clears_the_table() {
    let t = open();
    t.db.save_qa_runs(&[a_run("one")]);
    t.db.save_qa_runs(&[]);
    assert!(t.db.qa_runs().is_empty());
}

#[test]
fn a_lane_limit_round_trips_and_zero_is_no_cap() {
    // 0 means "no cap" everywhere else, so a stored 0 must not read back as a
    // cap of zero lanes — that would start nothing and look like a hang.
    let t = open();
    let capped = crate::qarun::QaRun {
        lane_limit: Some(6),
        ..a_run("capped")
    };
    t.db.save_qa_runs(std::slice::from_ref(&capped));
    assert_eq!(t.db.qa_runs()[0].lane_limit, Some(6));

    let zero = crate::qarun::QaRun {
        lane_limit: Some(0),
        ..a_run("capped")
    };
    t.db.save_qa_runs(std::slice::from_ref(&zero));
    assert_eq!(t.db.qa_runs()[0].lane_limit, None);
}

#[test]
fn shadow_mode_round_trips() {
    let t = open();
    let shadow = crate::qarun::QaRun {
        mode: crate::qarun::RunMode::Shadow,
        ..a_run("shadow")
    };
    t.db.save_qa_runs(std::slice::from_ref(&shadow));
    assert_eq!(t.db.qa_runs()[0].mode, crate::qarun::RunMode::Shadow);
}

#[test]
fn a_run_with_no_tasks_does_not_become_a_run_with_one() {
    // An empty id list splits into one empty string. Parsing that as a task
    // would give the run a phantom member.
    let t = open();
    let empty = crate::qarun::QaRun {
        task_ids: Vec::new(),
        spawned: Vec::new(),
        ..a_run("empty")
    };
    t.db.save_qa_runs(std::slice::from_ref(&empty));
    assert!(t.db.qa_runs()[0].task_ids.is_empty());
    assert!(t.db.qa_runs()[0].spawned.is_empty());
}
