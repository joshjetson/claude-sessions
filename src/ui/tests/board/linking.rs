//! Ported from `test/task-session-link.test.js`.
//!
//! "Go to the session's terminal" never worked in the Node app: the link lived
//! only in `state.taskSessions`, which the daemon builds when IT spawns a
//! session. After a daemon restart that map is empty, so every task reported
//! "no live session" even with a session plainly running for it. The durable
//! link is `session.task_id`, read from the transcript by the scanner.

use crate::ui::board::controller::{focus_task_terminal, task_sessions, SessionTarget};
use crate::ui::board::task_session;
use crate::ui::board::{go_to_task_session, resolve_notif_session};

use super::fixtures::*;

#[test]
fn finds_a_session_by_the_task_id_read_from_its_transcript() {
    // The case that was broken: no launch link at all.
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("sess-a", Some(5238), 1000)]);
    let found = task_session(state.sessions(), 5238, None).expect("a session working this task");
    assert_eq!(found.session_id, "sess-a");
}

#[test]
fn works_when_the_launch_links_are_empty_the_post_restart_condition() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("x", Some(4036), 1000)]);
    assert!(state.board.links.is_empty());
    assert!(task_session(state.sessions(), 4036, None).is_some());
}

#[test]
fn picks_the_most_recently_active_when_several_race_on_one_task() {
    // One real board had four sessions running at once on a single task.
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("old", Some(5238), 1000),
            live_session("newest", Some(5238), 9000),
            live_session("mid", Some(5238), 5000),
        ],
    );
    assert_eq!(
        task_session(state.sessions(), 5238, None)
            .unwrap()
            .session_id,
        "newest"
    );
    assert_eq!(task_sessions(state.sessions(), 5238).len(), 3);
}

#[test]
fn prefers_the_transcripts_own_timestamp_over_the_file_mtime() {
    // A transcript copied or touched by an archive pass has a fresh mtime and
    // nothing new in it; picking that one hands the revision to the wrong agent.
    let (_dir, mut state) = board_state();
    let mut newer_file = live_session("a", Some(1), 9000);
    newer_file.last_timestamp = Some("2026-01-01T00:00:00Z".to_string());
    let mut newer_entry = live_session("b", Some(1), 1000);
    newer_entry.last_timestamp = Some("2026-06-01T00:00:00Z".to_string());
    with_sessions(&mut state, vec![newer_file, newer_entry]);
    assert_eq!(
        task_session(state.sessions(), 1, None).unwrap().session_id,
        "b"
    );
}

#[test]
fn falls_back_to_the_recorded_link_when_the_transcript_names_no_task() {
    // Sessions started by hand carry no task URL; the old path still covers
    // them.
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("manual", None, 1000)]);
    with_link(&mut state, 777, running_link("/repo", "manual"));
    let link = state.board.link(777).cloned();
    assert_eq!(
        task_session(state.sessions(), 777, link.as_ref())
            .unwrap()
            .session_id,
        "manual"
    );
}

#[test]
fn a_task_with_no_session_answers_nothing_rather_than_the_wrong_one() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("other", Some(111), 1000)]);
    assert!(task_session(state.sessions(), 222, None).is_none());
}

#[test]
fn no_sessions_at_all_is_not_an_error() {
    let (_dir, state) = board_state();
    assert!(task_session(state.sessions(), 1, None).is_none());
    assert!(task_sessions(state.sessions(), 1).is_empty());
}

// The Node suite also pinned "string and number task ids both match", because
// the wire carried numbers and some call sites held strings. Rust's `i64` makes
// that unrepresentable — `Task.id` and `Session.task_id` are the same type all
// the way through — so the behaviour is preserved by construction. This test
// stands in for it: the comparison is on the value, not on a rendering of it.
#[test]
fn ids_are_compared_as_numbers_end_to_end() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("s", Some(5238), 1000)]);
    let found = task_session(state.sessions(), 5238, None).unwrap();
    assert_eq!(found.task_id, Some(5238));
    assert!(task_session(state.sessions(), 52380, None).is_none());
}

#[test]
fn going_to_a_session_names_the_project_it_is_in() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("sess-a", Some(5238), 1000)]);
    match go_to_task_session(state.sessions(), 5238, None) {
        SessionTarget::Session {
            session_id,
            project,
            ..
        } => {
            assert_eq!(session_id, "sess-a");
            assert_eq!(project, "repo");
        }
        other => panic!("expected a session target, got {other:?}"),
    }
}

#[test]
fn a_session_with_no_terminal_is_named_rather_than_denied() {
    // A detached agent is still running; saying "no live session" would be a
    // lie about the thing you can see on the row.
    let (_dir, mut state) = board_state();
    let mut detached = live_session("detached", Some(5238), 1000);
    detached.tty = Some("??".to_string());
    with_sessions(&mut state, vec![detached]);
    match focus_task_terminal(state.sessions(), 5238, None) {
        Err(SessionTarget::NoTerminal { session_id }) => assert_eq!(session_id, "detached"),
        other => panic!("expected NoTerminal, got {other:?}"),
    }
}

#[test]
fn focusing_reports_how_many_others_are_racing() {
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("a", Some(5238), 1000),
            live_session("b", Some(5238), 9000),
        ],
    );
    let target = focus_task_terminal(state.sessions(), 5238, None).expect("a terminal");
    assert_eq!(target.session_id, "b");
    assert_eq!(target.others, 2);
}

#[test]
fn a_notification_resolves_to_the_session_it_names_before_its_folder() {
    // cwd alone is ambiguous when several sessions run in one folder.
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("first", None, 1000),
            live_session("second", None, 2000),
        ],
    );
    let mut notif = notification("n1", None);
    notif.session_id = Some("first".to_string());
    let found = resolve_notif_session(state.sessions(), &notif, None).expect("a session");
    assert_eq!(found.session_id, "first");
}

#[test]
fn a_notification_falls_back_to_its_task_then_its_folder() {
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("unrelated", None, 5000),
            live_session("owner", Some(991001), 1000),
        ],
    );
    let by_task = resolve_notif_session(state.sessions(), &notification("n", Some(991001)), None)
        .expect("a session");
    assert_eq!(by_task.session_id, "owner");

    // No session id, no task: the folder is the last resort, and it is allowed
    // to be ambiguous — it answers with one of them rather than nothing.
    let by_cwd =
        resolve_notif_session(state.sessions(), &notification("n", None), None).expect("a session");
    assert_eq!(by_cwd.cwd, "/repo");
}
