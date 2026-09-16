//! A task archived with, and then resumed into, ANOTHER task's conversation.
//!
//! One task was archived with a different task's transcript — a different task,
//! in a different repo, for a different client. The caller's link was wrong, and
//! the archive trusted it. Resuming that archive then typed one task's revision
//! instructions into the other task's agent. Everything here is a layer of that
//! failure, asserted from below.

use super::*;

#[test]
fn a_transcript_opening_with_another_task_disqualifies_it() {
    let mut t = open();
    let other = t.transcript(Some(6440), "/repo/portal");
    assert!(t.belongs_to_other(other.path(), 4033));
}

#[test]
fn a_transcript_opening_with_this_task_does_not() {
    let mut t = open();
    let own = t.transcript(Some(4033), "/repo/portal");
    assert!(!t.belongs_to_other(own.path(), 4033));
}

#[test]
fn a_transcript_naming_no_task_is_allowed() {
    // Hand-started and merge-conflict sessions never carry the URL, and they
    // are legitimately archivable.
    let mut t = open();
    let plain = t.transcript(None, "/repo/portal");
    assert!(!t.belongs_to_other(plain.path(), 4033));
}

#[test]
fn an_unreadable_file_is_not_treated_as_another_tasks() {
    let mut t = open();
    assert!(!t.belongs_to_other(Path::new("/no/such/file.jsonl"), 4033));
}

#[test]
fn a_link_pointing_at_another_tasks_live_session_is_refused() {
    // The exact case: the recorded directory is the other task's repo, and the
    // handed-over session file is the other task's conversation.
    let mut t = open();
    let wrong = t.stray_transcript(Some(6440));
    let request = t.linked("/Users/nobody/dev/data-portal", &wrong);

    assert_eq!(t.archive_task(4033, &request), None);
    assert_eq!(
        t.db.get_task_archive(4033),
        None,
        "wrote a row for a refused archive"
    );
}

#[test]
fn no_archive_is_better_than_a_wrong_one() {
    let mut t = open();
    let wrong = t.stray_transcript(Some(6440));
    let request = t.linked("/Users/nobody/dev/data-portal", &wrong);
    t.archive_task(970003, &request);

    assert_eq!(
        t.meta(970003),
        None,
        "left a record of somebody else's conversation"
    );
    assert!(!Archive::new(&t.paths, &t.db).has_archive(970003));
}

#[test]
fn a_transcript_naming_no_task_is_still_archivable() {
    let mut t = open();
    let plain = t.stray_transcript(None);
    let request = t.linked("/repo/portal", &plain);
    assert!(t.archive_task(970002, &request).is_some());
}

#[test]
fn the_folder_fallback_skips_another_tasks_conversation() {
    // No link at all, so the newest file in the folder is taken — except that
    // the newest belongs to another task and the task-less one below it does
    // not. Sharing a folder is not evidence of anything.
    let mut t = open();
    let ours = t.aged(None, "/repo/portal", HOUR);
    t.transcript(Some(6440), "/repo/portal");

    let meta = t
        .archive_task(
            970004,
            &ArchiveRequest {
                cwd: "/repo/portal".to_string(),
                ..ArchiveRequest::default()
            },
        )
        .expect("nothing was archived");
    assert_eq!(meta.session_id, ours.session_id());
}
