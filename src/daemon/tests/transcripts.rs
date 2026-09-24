//! A whole tick on a machine whose process table this build cannot read.
//!
//! Everything downstream of the scan is shared with the process path — the
//! transcript cursor, the status machine, the task link, the grouping — so what
//! these pin is that a session with no pid survives all of it, and that its
//! status still moves with its transcript.

use std::time::Duration;

use super::*;
use crate::daemon::RefreshRequest;
use crate::scan::Discovery;

fn transcript_engine() -> TestEngine {
    engine_with(Setup {
        discovery: Some(Discovery::Transcripts),
        ..Setup::default()
    })
}

#[test]
fn a_tick_lists_the_transcripts_themselves_when_there_are_no_processes() {
    let mut harness = transcript_engine();
    let written = harness.transcript(6137, "/repo/app");
    harness.engine.refresh(RefreshRequest::default());

    let state = harness.state();
    let session = state
        .sessions
        .get(&written.session_id)
        .expect("the transcript was not listed");
    assert!(session.pids.is_empty(), "invented a pid");
    assert_eq!(session.tty, None);
    assert_eq!(session.lstart, None);
    // The cwd came out of the transcript's own head, and everything keyed on it
    // still works: the project grouping and the task link.
    assert_eq!(session.cwd, "/repo/app");
    assert_eq!(session.task_id, Some(6137));
    assert_eq!(
        state.sessions.by_project().keys().collect::<Vec<_>>(),
        vec!["repo/app"]
    );
    // Written a moment ago, so the file is being appended to right now.
    assert_eq!(session.status, SessionStatus::Working);
}

#[test]
fn the_status_machine_still_reads_the_mtime_and_the_last_entry() {
    // A prompt with no reply for ten minutes is a turn that hung, so the
    // session reads idle — the same judgement a paired session gets, from the
    // same two facts, neither of which is a process. The test transcript
    // carries no timestamps, so the mtime is the clock. (It used to read
    // "awaiting input", which is wrong: nobody waits on the person after the
    // person typed.)
    let mut harness = transcript_engine();
    let written = harness.aged_transcript(6137, "/repo/app", 10 * MINUTE);
    harness.engine.refresh(RefreshRequest::default());

    let state = harness.state();
    let session = state.sessions.get(&written.session_id).expect("not listed");
    assert_eq!(session.status, SessionStatus::Idle);
}

#[test]
fn a_hook_state_file_overrides_an_older_transcript() {
    // A PermissionRequest recorded by `claude-sessions hook` after the prompt:
    // the transcript alone says "working", the hook says "awaiting".
    let mut harness = transcript_engine();
    let written = harness.transcript(6137, "/repo/app");
    let action = crate::hook_state::parse_hook_input(
        serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "session_id": written.session_id,
            "tool_name": "Bash",
        })
        .to_string()
        .as_bytes(),
        chrono::Utc::now(),
    );
    crate::hook_state::apply(&harness.paths, &action).expect("hook state written");
    harness.engine.refresh(RefreshRequest::default());

    let state = harness.state();
    let session = state.sessions.get(&written.session_id).expect("not listed");
    assert_eq!(session.status, SessionStatus::Awaiting);
    // The watcher sees the hook and raises the prompt notification at once.
    assert!(
        state
            .notifications
            .iter()
            .any(|n| n.title.contains("may be waiting on a prompt")),
        "{:?}",
        state
            .notifications
            .iter()
            .map(|n| n.title.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_transcript_from_last_week_is_not_a_live_session() {
    const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    let mut harness = transcript_engine();
    harness.aged_transcript(6137, "/repo/old", WEEK);
    let fresh = harness.transcript(6138, "/repo/app");
    harness.engine.refresh(RefreshRequest::default());

    let state = harness.state();
    assert_eq!(
        state
            .sessions
            .iter()
            .map(|s| s.session_id.clone())
            .collect::<Vec<_>>(),
        vec![fresh.session_id]
    );
}

#[test]
fn the_process_path_is_unchanged_on_the_same_store() {
    // The gate is the capability flag, not the filesystem: the identical
    // transcript with no process behind it is not a session on a machine that
    // can read its process table.
    let mut harness = engine();
    harness.transcript(6137, "/repo/app");
    harness.engine.refresh(RefreshRequest::default());
    assert_eq!(harness.state().sessions.len(), 0);
}
