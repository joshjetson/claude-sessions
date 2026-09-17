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
    // Ten minutes of silence after a user entry is the session waiting on its
    // owner, not working — the same judgement a paired session gets, from the
    // same two facts, neither of which is a process.
    let mut harness = transcript_engine();
    let written = harness.aged_transcript(6137, "/repo/app", 10 * MINUTE);
    harness.engine.refresh(RefreshRequest::default());

    let state = harness.state();
    let session = state.sessions.get(&written.session_id).expect("not listed");
    assert_eq!(session.status, SessionStatus::AwaitingInput);
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
