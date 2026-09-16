//! The helper subcommands: `notify`, `done` and `blocked`.
//!
//! These are the three the Node app got wrong. `bin/done.js` and friends joined
//! their marker directory onto `homedir()` themselves instead of going through
//! the runtime paths, so `CLAUDE_SESSIONS_HOME` moved the dashboard's state and
//! left the agents writing completion markers into the real one. Every path
//! here comes from a [`Paths`], which is what makes the isolation real.

use std::fs;
use std::time::{Duration, SystemTime};

use super::*;
use crate::cli::{notify_text, split_questions, write_blocked_marker, write_done_marker};
use crate::daemon::{BlockedMarker, DoneMarker};
use crate::paths::PathEnv;

/// Paths as `CLAUDE_SESSIONS_HOME=<root>/elsewhere` would resolve them, next to
/// a home directory that must stay untouched.
fn relocated(root: &std::path::Path) -> Paths {
    Paths::resolve(
        root,
        &PathEnv {
            sessions_home: Some(root.join("elsewhere")),
            ..PathEnv::default()
        },
    )
}

#[test]
fn done_writes_its_marker_under_the_relocated_home() {
    let dir = tempfile::tempdir().unwrap();
    let paths = relocated(dir.path());

    let marker = write_done_marker(
        &paths,
        4242,
        "/repo/app",
        "Root cause: the guard ran early.",
    )
    .expect("the marker was not written");

    assert_eq!(marker, paths.done_marker(4242));
    assert!(marker.starts_with(dir.path().join("elsewhere")));
    assert!(
        !dir.path().join(".claude-sessions").exists(),
        "the real runtime directory was written to — this is the Node bug"
    );
    assert!(paths.is_isolated());
}

#[test]
fn a_done_marker_is_exactly_what_the_daemon_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let paths = relocated(dir.path());
    write_done_marker(&paths, 4242, "/repo/app", "all done").unwrap();

    let raw = fs::read_to_string(paths.done_marker(4242)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["taskId"], 4242);
    assert_eq!(value["cwd"], "/repo/app");
    assert_eq!(value["summary"], "all done");
    assert!(value["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')));

    let parsed: DoneMarker = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed.task_id, 4242);
}

#[test]
fn a_summary_is_capped_where_the_daemon_caps_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = relocated(dir.path());
    write_done_marker(&paths, 1, "/repo", &"s".repeat(9000)).unwrap();
    let parsed: DoneMarker =
        serde_json::from_str(&fs::read_to_string(paths.done_marker(1)).unwrap()).unwrap();
    assert_eq!(parsed.summary.len(), 8000);
}

#[test]
fn blocked_splits_its_questions_on_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let paths = relocated(dir.path());
    let questions = split_questions("which key? | which environment? |  | staging or prod?");
    write_blocked_marker(&paths, 4243, "/repo/app", questions).unwrap();

    let parsed: BlockedMarker =
        serde_json::from_str(&fs::read_to_string(paths.blocked_marker(4243)).unwrap()).unwrap();
    assert_eq!(
        parsed.questions,
        ["which key?", "which environment?", "staging or prod?"],
        "empty segments are dropped and each question is trimmed"
    );
    assert_eq!(parsed.task_id, 4243);
    assert!(split_questions("").is_empty());
}

#[test]
fn a_marker_the_cli_wrote_is_one_the_engine_acts_on() {
    // The two halves have to agree about the filename, the field names and the
    // directory — this is the only test that proves they do.
    let mut harness = engine();
    harness.transcript(4242, "/repo/app");
    write_done_marker(&harness.paths, 4242, "/repo/app", "shipped").unwrap();
    let marker = harness.paths.done_marker(4242);
    fs::File::options()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(1))
        .unwrap();

    harness.inner().poll_markers();
    harness.inner().join_workers();

    assert!(!marker.exists(), "the marker was not consumed");
    assert!(harness.state().done_tasks.contains(&4242));
}

#[test]
fn the_first_bare_argument_is_the_title_and_the_rest_is_the_message() {
    assert_eq!(
        notify_text(
            None,
            None,
            vec![
                "Need a decision".to_string(),
                "Postgres".to_string(),
                "or SQLite?".to_string(),
            ],
        ),
        (
            "Need a decision".to_string(),
            "Postgres or SQLite?".to_string()
        )
    );
    // A flag wins, and the positionals then fill only what is missing.
    assert_eq!(
        notify_text(
            Some("Blocked".to_string()),
            None,
            vec!["which key?".to_string()]
        ),
        ("Blocked".to_string(), "which key?".to_string())
    );
    assert_eq!(
        notify_text(None, Some("body".to_string()), vec!["title".to_string()]),
        ("title".to_string(), "body".to_string())
    );
    assert_eq!(
        notify_text(None, None, Vec::new()),
        (String::new(), String::new())
    );
}
