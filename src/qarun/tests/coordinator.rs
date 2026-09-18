//! Recognising a run's coordinator among the sessions that are running.
//!
//! The daemon does this job for tasks and keys on a task id. A coordinator
//! holds no task, so the match runs on the folder it was launched in and on
//! being new — and every rule below is one way that match can go wrong.
//!
//! The asymmetry that shapes all of it: matching NOTHING costs a run one more
//! second showing "starting". Matching the WRONG session makes the dashboard
//! call a QA pass the coordinator, and every question then gets typed into a
//! session that was told not to answer questions.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::qarun::{resolve_coordinator, CoordinatorPending};
use crate::types::{Session, SessionStatus};

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
    }
}

fn pending(known: &[&str]) -> CoordinatorPending {
    CoordinatorPending {
        cwd: "/repo/alpha".to_string(),
        known_session_ids: known.iter().map(|id| id.to_string()).collect(),
    }
}

#[test]
fn the_new_session_in_the_launch_folder_is_the_coordinator() {
    let sessions = [session("old", "/repo/alpha"), session("new", "/repo/alpha")];
    assert_eq!(
        resolve_coordinator(&pending(&["old"]), sessions.iter()),
        Some("new".to_string())
    );
}

#[test]
fn a_session_that_already_existed_is_never_the_coordinator() {
    // The whole point of recording the known ids at launch. Without this the
    // reviewer's own session, sitting in the same repo, gets claimed.
    let sessions = [session("old", "/repo/alpha")];
    assert_eq!(
        resolve_coordinator(&pending(&["old"]), sessions.iter()),
        None
    );
}

#[test]
fn a_session_in_another_folder_is_never_the_coordinator() {
    let sessions = [session("new", "/repo/bravo")];
    assert_eq!(resolve_coordinator(&pending(&[]), sessions.iter()), None);
}

#[test]
fn a_session_holding_a_task_is_never_the_coordinator() {
    // This is the QA sessions. The run starts the coordinator and then starts
    // them in the same folder seconds later, so without this rule the first QA
    // pass to appear gets claimed as the watcher.
    let mut qa = session("new", "/repo/alpha");
    qa.task_id = Some(4101);
    assert_eq!(resolve_coordinator(&pending(&[]), [qa].iter()), None);
}

#[test]
fn a_process_with_no_transcript_yet_is_not_claimed() {
    // A `claude` process writes nothing for its first seconds and shows up as a
    // placeholder with no file. It matches every launch in the folder, which is
    // exactly how the task launch queue once linked seven tasks to one session.
    let mut starting = session("new", "/repo/alpha");
    starting.session_file = None;
    starting.starting = true;
    assert_eq!(resolve_coordinator(&pending(&[]), [starting].iter()), None);
}

#[test]
fn an_empty_folder_matches_nothing() {
    // A run whose folder never resolved records no folder. Matching on "" would
    // otherwise claim the first session in the list.
    let empty = CoordinatorPending {
        cwd: String::new(),
        known_session_ids: Vec::new(),
    };
    let sessions = [session("new", "/repo/alpha")];
    assert_eq!(resolve_coordinator(&empty, sessions.iter()), None);
}
