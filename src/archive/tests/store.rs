//! Archiving itself: what is copied, what is recorded, and which round wins.

use super::*;

#[test]
fn a_tasks_own_transcript_is_archived() {
    let mut t = open();
    let own = t.transcript(Some(970001), "/repo/portal");
    let request = t.linked("/repo/portal", own.path());

    let meta = t
        .archive_task(970001, &request)
        .expect("refused its own transcript");
    assert_eq!(meta.session_id, own.session_id());
    assert_eq!(meta.cwd, "/repo/portal");
    assert_eq!(meta.session_file, own.path().to_string_lossy());
    assert_eq!(t.db.get_task_archive(970001).as_ref(), Some(&meta));

    // The copy, the file record and the index row all land together.
    let copy = t.archive_path(970001).expect("no archived transcript");
    assert_eq!(
        fs::read_to_string(&copy).unwrap(),
        fs::read_to_string(own.path()).unwrap()
    );
    assert!(t.paths.task_dir(970001).join("meta.json").exists());
    assert_eq!(
        t.db.task_session(970001).map(|row| row.session_file),
        Some(own.path().to_string_lossy().into_owned())
    );
}

#[test]
fn round_two_replaces_round_one() {
    // A task that goes back to QA writes a new transcript each round. The
    // archive is the conversation to resume, not a history — one task kept a
    // 167-line stub while its real QA session held 2963 lines.
    let mut t = open();
    // Rooted for the platform: the search absolutises before it encodes.
    let repo = rooted("/repo/atlas");
    let round1 = t.aged(Some(970101), &repo, HOUR);
    let request = t.linked(&repo, round1.path());
    assert_eq!(
        t.archive_task(970101, &request).map(|m| m.session_id),
        Some(round1.session_id()),
        "round 1 was not archived"
    );

    let round2 = t.transcript(Some(970101), &repo);
    let request = t.linked(&repo, round2.path());
    assert_eq!(
        t.archive_task(970101, &request).map(|m| m.session_id),
        Some(round2.session_id()),
        "the archive stayed on round 1"
    );

    assert_eq!(t.meta(970101).unwrap().session_id, round2.session_id());
    assert_eq!(
        t.db.list_archived_task_ids(),
        vec![970101],
        "round 2 must replace round 1, not sit beside it"
    );
}

#[test]
fn the_newest_of_the_recorded_and_signed_off_folders_wins() {
    // The three searches race: the folder the task was launched in, the folder
    // the agent signed off from, and everywhere. Whichever holds the newest
    // transcript is the conversation to resume.
    let mut t = open();
    t.aged(Some(970105), "/repo/recorded", HOUR);
    let signed_off = t.transcript(Some(970105), "/repo/signed-off");

    let meta = t
        .archive_task(
            970105,
            &ArchiveRequest {
                cwd: "/repo/recorded".to_string(),
                alt_cwd: "/repo/signed-off".to_string(),
                ..ArchiveRequest::default()
            },
        )
        .expect("nothing was archived");
    assert_eq!(meta.session_id, signed_off.session_id());
    assert_eq!(
        meta.cwd, "/repo/signed-off",
        "recorded the folder the task no longer runs in"
    );
}

#[test]
fn a_transcript_that_is_gone_archives_nothing() {
    let mut t = open();
    let request = t.linked("/repo/portal", Path::new("/no/such/file.jsonl"));
    assert_eq!(t.archive_task(970006, &request), None);
    assert!(!Archive::new(&t.paths, &t.db).has_archive(970006));
}

#[test]
fn a_stale_link_is_replaced_by_the_transcript_that_names_the_task() {
    // The link says one thing, the transcript head says another. The head wins:
    // it is the only link that survives a daemon restart.
    let mut t = open();
    let stale = t.stray_transcript(None);
    let own = t.transcript(Some(970007), "/repo/portal");
    let request = t.linked("/repo/portal", &stale);

    let meta = t.archive_task(970007, &request).unwrap();
    assert_eq!(meta.session_id, own.session_id());
}

#[test]
fn the_record_falls_back_to_the_file_when_the_database_has_no_row() {
    // meta.json is the durable copy: an archive written before the import, or
    // one whose row was lost with the database, still resumes.
    let mut t = open();
    let own = t.transcript(Some(970008), "/repo/portal");
    let request = t.linked("/repo/portal", own.path());
    let meta = t.archive_task(970008, &request).unwrap();

    t.db.exec("drop_row", |conn| {
        conn.execute("DELETE FROM task_archive", [])?;
        Ok(())
    });
    assert_eq!(t.db.get_task_archive(970008), None, "precondition: no row");

    let archive = Archive::new(&t.paths, &t.db);
    assert!(archive.has_archive(970008));
    assert_eq!(archive.task_meta(970008), Some(meta));
    assert_eq!(archive.list_archived_task_ids(), vec![970008]);
}
