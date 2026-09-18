//! Recognising a run's coordinator among the sessions that are running.
//!
//! It is recognised by `CLAUDE_SESSIONS_RUN_ID`, which the launch exports into
//! the process environment and the scanner reads back from `ps -E`.
//!
//! The version this replaced guessed: "a new session, in the launch folder,
//! holding no task". The run's OWN QA sessions satisfy all three for the moment
//! between writing a transcript and that transcript being read for a task URL,
//! and they start in the same folder seconds later — uncapped, seven at once.
//! Losing that race meant the dashboard calling a QA pass the coordinator and
//! typing every question into a session told not to answer questions.
//!
//! So the tests below are mostly about what must NOT match.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::qarun::coordinator_of;
use crate::types::{Session, SessionStatus};

const RUN: &str = "Project::Quality Assurance";

fn session(id: &str, cwd: &str) -> Session {
    Session {
        session_id: id.to_string(),
        pids: vec![1],
        cwd: cwd.to_string(),
        tty: Some("ttys001".to_string()),
        lstart: None,
        session_file: Some(PathBuf::from(format!("/transcripts/{id}.jsonl"))),
        session_mtime: SystemTime::UNIX_EPOCH,
        session_size: None,
        status: SessionStatus::Working,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
        run_id: None,
    }
}

fn coordinator(id: &str, run: &str) -> Session {
    Session {
        run_id: Some(run.to_string()),
        ..session(id, "/repo/alpha")
    }
}

fn id_of(found: Option<&Session>) -> Option<&str> {
    found.map(|s| s.session_id.as_str())
}

#[test]
fn the_session_carrying_the_run_id_is_the_coordinator() {
    let sessions = [session("qa", "/repo/alpha"), coordinator("coord", RUN)];
    assert_eq!(id_of(coordinator_of(RUN, sessions.iter())), Some("coord"));
}

#[test]
fn a_qa_session_in_the_same_folder_is_never_mistaken_for_it() {
    // The race this whole mechanism exists to remove. Same folder, no task id
    // yet, brand new — and still not the coordinator, because it carries no
    // run id and nothing but a coordinator does.
    let mut racing = session("qa", "/repo/alpha");
    racing.task_id = None;
    assert_eq!(id_of(coordinator_of(RUN, [racing].iter())), None);
}

#[test]
fn seven_qa_sessions_do_not_make_a_coordinator_between_them() {
    // Uncapped is the normal setting, so the realistic case is a crowd.
    let crowd: Vec<Session> = (0..7)
        .map(|i| session(&format!("qa{i}"), "/repo/alpha"))
        .collect();
    assert_eq!(id_of(coordinator_of(RUN, crowd.iter())), None);
}

#[test]
fn another_runs_coordinator_is_not_this_runs_coordinator() {
    // Two runs open at once, both with a coordinator, both in the same repo.
    let other = coordinator("other", "Project::Ready for QA");
    let mine = coordinator("mine", RUN);
    assert_eq!(
        id_of(coordinator_of(RUN, [other, mine].iter())),
        Some("mine")
    );
}

#[test]
fn the_folder_no_longer_matters() {
    // A coordinator launched somewhere unexpected — a picker, a worktree, a
    // folder the project resolution got wrong — is still found. The old match
    // would have missed it entirely and reported "no coordinator".
    let elsewhere = Session {
        cwd: "/somewhere/else".to_string(),
        ..coordinator("coord", RUN)
    };
    assert_eq!(
        id_of(coordinator_of(RUN, [elsewhere].iter())),
        Some("coord")
    );
}

#[test]
fn a_coordinator_is_found_before_it_writes_a_transcript() {
    // The environment is set before the process starts, so a coordinator is
    // identifiable while it is still a `starting-<pid>` placeholder. The old
    // match had to wait for a transcript and reported "no coordinator" until
    // one appeared.
    let starting = Session {
        session_file: None,
        starting: true,
        status: SessionStatus::Starting,
        ..coordinator("starting-900", RUN)
    };
    assert_eq!(
        id_of(coordinator_of(RUN, [starting].iter())),
        Some("starting-900")
    );
}

#[test]
fn an_empty_run_id_matches_nothing() {
    // Otherwise a run with no id would claim every session that has no run id.
    let sessions = [session("qa", "/repo/alpha")];
    assert_eq!(id_of(coordinator_of("", sessions.iter())), None);
}

#[test]
fn a_dead_coordinator_simply_stops_being_found() {
    // Nothing is cached, so there is no stale entry to invalidate: the run
    // reports "no coordinator" the moment the process is gone from the scan.
    let sessions: [Session; 0] = [];
    assert_eq!(id_of(coordinator_of(RUN, sessions.iter())), None);
}

// --- carrying a run id through a process environment -------------------------
//
// `ps -E` prints the whole environment as ONE space-separated line, so a value
// containing a space cannot be read back from it: the reader stops at the space
// and gets half an id. Every run id has a space in it, because stage names do.

#[test]
fn a_run_id_survives_the_trip_through_an_environment() {
    let id = "Project::Quality Assurance";
    let encoded = crate::qarun::encode_run_id(id);
    assert!(
        !encoded.contains(' '),
        "the encoded id still has a space in it: {encoded:?}"
    );
    assert_eq!(crate::qarun::decode_run_id(&encoded), id);
}

#[test]
fn the_value_that_failed_on_a_real_machine() {
    // Observed: `CLAUDE_SESSIONS_RUN_ID='Aurora::Quality` in `ps -E` output,
    // for a run whose id was "Aurora::Quality Assurance". The reader stopped at
    // the space, kept the opening quote, and matched nothing — so the dashboard
    // said "no coordinator" while the coordinator was running.
    let id = "Aurora::Quality Assurance";
    assert_eq!(
        crate::qarun::decode_run_id(&crate::qarun::encode_run_id(id)),
        id
    );
}

#[test]
fn a_quoted_value_is_still_read() {
    // Some platforms report the quoted form the shell was handed rather than
    // the value it ended up exporting.
    assert_eq!(
        crate::qarun::decode_run_id("'Project::Quality%20Assurance'"),
        "Project::Quality Assurance"
    );
}

#[test]
fn a_percent_in_a_run_id_does_not_invent_a_space() {
    // Encoding `%` first is what stops "100%20s" — a legitimate name — from
    // decoding into "100 s".
    let id = "Project::100%20s";
    assert_eq!(
        crate::qarun::decode_run_id(&crate::qarun::encode_run_id(id)),
        id
    );
}

#[test]
fn a_tab_is_encoded_too() {
    // Any whitespace breaks the `ps -E` line, not only a space.
    let id = "Project::A\tB";
    let encoded = crate::qarun::encode_run_id(id);
    assert!(!encoded.chars().any(char::is_whitespace));
    assert_eq!(crate::qarun::decode_run_id(&encoded), id);
}

#[test]
fn a_value_that_was_never_encoded_is_returned_unchanged() {
    // Nothing should break if the encoding is ever removed or bypassed.
    assert_eq!(
        crate::qarun::decode_run_id("Project::Ready"),
        "Project::Ready"
    );
}
