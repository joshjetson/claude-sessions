//! `ensure_live_session`: what is safe to resume, and what is repaired first.

use super::*;

#[test]
fn a_record_pointing_at_another_tasks_transcript_is_never_handed_back() {
    // Exactly the state the cross-linked task was left in: a good-looking row
    // pointing at a transcript belonging to another task. This is what made a
    // revision get typed into an unrelated agent.
    let mut t = open();
    let wrong = t.stray_transcript(Some(6440));
    t.record(
        970010,
        "/Users/nobody/dev/data-portal",
        "sess-6440-1",
        &wrong,
    );

    assert_eq!(
        t.ensure_live(970010),
        None,
        "offered another task's session to resume"
    );
}

#[test]
fn a_correct_record_is_still_returned() {
    let mut t = open();
    let own = t.transcript(Some(970011), "/repo/portal");
    t.record(970011, "/repo/portal", &own.session_id(), own.path());

    assert_eq!(
        t.ensure_live(970011).map(|m| m.session_id),
        Some(own.session_id())
    );
}

#[test]
fn a_task_with_no_archive_has_nothing_to_resume() {
    let mut t = open();
    assert_eq!(t.ensure_live(970012), None);
}

#[test]
fn a_record_a_later_round_left_behind_is_repaired() {
    // Round 1 in the repo, round 3 in the QA folder the task was never recorded
    // in. Resuming must land in the conversation that is actually current.
    let mut t = open();
    let round1 = t.aged(Some(970114), "/repo/commonwealth", HOUR);
    t.record(
        970114,
        "/repo/commonwealth",
        &round1.session_id(),
        round1.path(),
    );
    let round2 = t.transcript(Some(970114), "/Users/nobody/Desktop/QAden/task-970114-qa");

    let meta = t.ensure_live(970114).expect("nothing to resume");
    assert_eq!(
        meta.session_id,
        round2.session_id(),
        "stayed on the superseded round"
    );
    assert_eq!(
        meta.cwd, "/Users/nobody/Desktop/QAden/task-970114-qa",
        "kept the folder the task no longer runs in"
    );
    // The repair is durable: the row, the file and the archived copy all move.
    assert_eq!(t.meta(970114).unwrap().session_id, round2.session_id());
    assert!(t.archive_path(970114).is_some());
}

#[test]
fn a_transcript_claude_cleaned_up_is_restored_from_the_archive() {
    let mut t = open();
    let own = t.transcript(Some(970013), "/repo/portal");
    let request = t.linked("/repo/portal", own.path());
    t.archive_task(970013, &request).unwrap();
    let contents = fs::read_to_string(own.path()).unwrap();

    // Claude Code rotates its project directory out from under us.
    fs::remove_dir_all(own.path().parent().unwrap()).unwrap();
    assert!(
        !own.path().exists(),
        "precondition: the live transcript is gone"
    );

    let meta = t.ensure_live(970013).expect("nothing to resume");
    assert_eq!(meta.session_id, own.session_id());
    assert!(
        own.path().exists(),
        "`claude --resume` would not find the session"
    );
    assert_eq!(fs::read_to_string(own.path()).unwrap(), contents);
}

#[test]
fn a_record_with_no_session_file_resolves_one_from_its_folder() {
    // Written by an import, or by a Node build that only knew the cwd: the live
    // path is derived from the recorded folder and the session id.
    let mut t = open();
    let own = t.transcript(Some(970014), "/repo/portal");
    let contents = fs::read_to_string(own.path()).unwrap();
    fs::create_dir_all(t.paths.task_dir(970014)).unwrap();
    fs::write(
        t.paths.task_dir(970014).join(own.file.name.clone()),
        &contents,
    )
    .unwrap();
    t.record(970014, "/repo/portal", &own.session_id(), Path::new(""));
    fs::remove_file(own.path()).unwrap();

    assert_eq!(
        t.ensure_live(970014).map(|m| m.session_id),
        Some(own.session_id())
    );
    assert!(
        own.path().exists(),
        "did not restore into the recorded folder"
    );
}
