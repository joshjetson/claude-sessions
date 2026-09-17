//! The task-to-session index (Big-O mandate #4): one SELECT in place of reading
//! every project directory, stat-ing every transcript in it and reading a head
//! from up to four hundred of them — which Node did on every session that ended.

use super::*;
use crate::db::TaskSession;
use crate::util::iso_now;

fn index_row(t: &TestArchive, task_id: i64, file: &Path, cwd: &str) {
    t.db.put_task_session(&TaskSession {
        task_id,
        session_file: file.to_string_lossy().into_owned(),
        cwd: cwd.to_string(),
        updated_at: iso_now(),
    });
}

#[test]
fn an_indexed_task_is_found_without_reading_a_single_transcript() {
    let mut t = open();
    let indexed = t.transcript(Some(6688), "/repo/portal");
    // Decoys: the scan would have to read every one of these heads.
    t.transcript(Some(4033), "/repo/other");
    t.transcript(None, "/repo/other");
    t.transcript(Some(6440), "/Users/nobody/Desktop/QAden/task-6440-qa");
    index_row(&t, 6688, indexed.path(), "/repo/portal");

    let before = t.refs.reads();
    let found = t.find_anywhere(6688).expect("the indexed row was not used");
    assert_eq!(found.path(), indexed.path());
    assert_eq!(found.cwd, "/repo/portal");
    // One head: the row's own file, checked before it is trusted. The decoys —
    // which the scan would have had to read — are never opened.
    assert_eq!(
        t.refs.reads(),
        before + 1,
        "the fast path read more than the one head it verifies, so it scanned"
    );
}

/// A row can be written the moment a task is linked, which can be before the
/// session it names has written its opening prompt. If that transcript turns
/// out to be a sibling task's, the row is wrong — and trusting it would copy
/// another task's conversation into this task's archive. Node re-read the head
/// of every candidate (`archive.js:139`); the index has to do the same for the
/// one candidate it returns.
#[test]
fn an_index_row_naming_another_tasks_transcript_is_refused() {
    let mut t = open();
    let wrong = t.transcript(Some(4033), "/repo/portal");
    let right = t.transcript(Some(6688), "/repo/portal");
    index_row(&t, 6688, wrong.path(), "/repo/portal");

    let found = t.find_anywhere(6688).expect("the scan did not run");
    assert_eq!(
        found.path(),
        right.path(),
        "the fast path handed back another task's transcript"
    );
}

/// A transcript with no task reference at all stays valid: sessions started by
/// hand and merge-conflict sessions carry none, and the index is how they are
/// found again.
#[test]
fn an_index_row_naming_an_unattributed_transcript_is_still_used() {
    let mut t = open();
    let plain = t.transcript(None, "/repo/portal");
    index_row(&t, 6688, plain.path(), "/repo/portal");

    let found = t.find_anywhere(6688).expect("the row was refused");
    assert_eq!(found.path(), plain.path());
}

#[test]
fn a_row_whose_transcript_is_gone_falls_back_to_the_scan() {
    let mut t = open();
    let stale = t.transcript(Some(6688), "/repo/portal");
    index_row(&t, 6688, stale.path(), "/repo/portal");
    fs::remove_file(stale.path()).unwrap();
    let current = t.transcript(Some(6688), "/Users/nobody/Desktop/QAden/task-6688-qa");

    let found = t.find_anywhere(6688).expect("the scan did not run");
    assert_eq!(found.path(), current.path());
    assert!(
        t.refs.reads() > 0,
        "claimed to have scanned without reading anything"
    );
}

#[test]
fn a_scan_records_what_it_found_so_the_next_lookup_is_free() {
    let mut t = open();
    let moved = t.transcript(Some(6688), "/Users/nobody/Desktop/QAden/task-6688-qa");

    assert_eq!(
        t.find_anywhere(6688).map(|f| f.file.path),
        Some(moved.file.path.clone())
    );
    let row =
        t.db.task_session(6688)
            .expect("the resolution was not indexed");
    assert_eq!(row.session_file, moved.path().to_string_lossy());
    assert_eq!(row.cwd, "/Users/nobody/Desktop/QAden/task-6688-qa");

    let after_scan = t.refs.reads();
    assert_eq!(
        t.find_anywhere(6688).map(|f| f.file.path),
        Some(moved.file.path)
    );
    assert_eq!(
        t.refs.reads(),
        after_scan,
        "the second lookup scanned again"
    );
}

#[test]
fn archiving_indexes_the_transcript_it_settled_on() {
    let mut t = open();
    let own = t.transcript(Some(970120), "/repo/portal");
    let request = t.linked("/repo/portal", own.path());
    t.archive_task(970120, &request).unwrap();

    assert_eq!(
        t.db.task_session(970120).map(|row| row.session_file),
        Some(own.path().to_string_lossy().into_owned())
    );
    // …and the reverse index answers the one-session-one-task question the
    // pending-launch rules ask.
    assert_eq!(
        t.db.task_for_session_file(&own.path().to_string_lossy())
            .map(|row| row.task_id),
        Some(970120)
    );
}

#[test]
fn a_task_with_no_transcript_anywhere_is_not_indexed() {
    let mut t = open();
    assert_eq!(t.find_anywhere(970121), None);
    assert_eq!(
        t.db.task_session(970121),
        None,
        "indexed a transcript that does not exist"
    );
}
