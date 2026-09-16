//! Ported from `test/task-session-linking.test.js` — the ways a task and a
//! transcript came apart.
//!
//! Eight tasks were started from one folder inside a minute. A `claude`
//! process writes no transcript for its first seconds, so the scanner showed
//! each as a `starting-<pid>` placeholder: no file, no task id. That
//! placeholder satisfied every launch, the single pending slot held only the
//! newest launch, and nothing re-checked the link once the real transcript
//! arrived — seven tasks ended up pointing at one placeholder.

use std::time::{Duration, SystemTime};

use super::*;
use crate::daemon::{PendingLaunch, PendingRequest, TaskLink, TaskLinkStatus};
use crate::db::TaskSession;
use crate::util::iso_now;

/// What `set_pending` queues, without the clock.
fn queue(harness: &TestEngine, entries: &[(&str, Option<i64>)]) {
    let now = SystemTime::now();
    harness.state().pending = entries
        .iter()
        .map(|(cwd, task_id)| PendingLaunch {
            cwd: cwd.to_string(),
            task_id: *task_id,
            launched_at: now,
            known_session_ids: Default::default(),
        })
        .collect();
}

fn link(harness: &TestEngine, sessions: Vec<Session>) {
    harness
        .inner()
        .link_pending_sessions(&index(sessions), SystemTime::now());
}

fn live(session_id: &str, task_id: Option<i64>) -> Session {
    Session {
        task_id,
        ..a_session(session_id, "/repo/one")
    }
}

fn linked_session(harness: &TestEngine, task_id: i64) -> Option<String> {
    harness
        .state()
        .task_sessions
        .get(&task_id)
        .map(|link| link.session_id.clone())
}

// --- a launch never claims a starting-<pid> placeholder ---------------------

#[test]
fn a_placeholder_leaves_the_task_unlinked_rather_than_wrongly_linked() {
    let harness = engine();
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(&harness, vec![a_placeholder(72404, "/repo/one")]);
    assert_eq!(
        linked_session(&harness, 6270),
        None,
        "linked a task to a placeholder"
    );
}

#[test]
fn the_launch_stays_queued_so_the_real_transcript_still_links() {
    let harness = engine();
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(&harness, vec![a_placeholder(72404, "/repo/one")]);
    assert_eq!(
        harness.state().pending.len(),
        1,
        "dropped the launch on a placeholder"
    );

    link(&harness, vec![live("real", Some(6270))]);
    assert_eq!(linked_session(&harness, 6270).as_deref(), Some("real"));
    assert!(
        harness.state().pending.is_empty(),
        "kept a launch that resolved"
    );
}

#[test]
fn the_exact_collision_two_tasks_one_folder_one_placeholder() {
    let harness = engine();
    queue(
        &harness,
        &[("/repo/one", Some(6270)), ("/repo/one", Some(6272))],
    );
    link(&harness, vec![a_placeholder(72404, "/repo/one")]);
    assert!(
        harness.state().task_sessions.is_empty(),
        "two tasks claimed the same placeholder"
    );
}

// --- a session belongs to one task only -------------------------------------

#[test]
fn a_session_another_task_already_holds_is_refused() {
    let harness = engine();
    harness.state().task_sessions.insert(
        6270,
        TaskLink {
            cwd: "/repo/one".to_string(),
            session_id: "sess-a".to_string(),
            ..TaskLink::default()
        },
    );
    queue(&harness, &[("/repo/one", Some(6272))]);
    link(&harness, vec![live("sess-a", None)]);
    assert_eq!(
        linked_session(&harness, 6272),
        None,
        "handed one session to two tasks"
    );
}

#[test]
fn a_session_the_index_says_belongs_elsewhere_is_refused_after_a_restart() {
    // The in-memory links are empty after a restart; the index is not. Without
    // this the one-session-one-task rule only held while the daemon that made
    // the link was still running.
    let harness = engine();
    harness.engine.db().put_task_session(&TaskSession {
        task_id: 6270,
        session_file: "/transcripts/sess-a.jsonl".to_string(),
        cwd: "/repo/one".to_string(),
        updated_at: iso_now(),
    });
    queue(&harness, &[("/repo/one", Some(6272))]);
    link(&harness, vec![live("sess-a", None)]);
    assert_eq!(linked_session(&harness, 6272), None);
}

#[test]
fn two_launches_in_one_folder_take_one_session_each() {
    let harness = engine();
    queue(
        &harness,
        &[("/repo/one", Some(6270)), ("/repo/one", Some(6272))],
    );
    link(
        &harness,
        vec![live("sess-a", Some(6270)), live("sess-b", Some(6272))],
    );
    assert_eq!(linked_session(&harness, 6270).as_deref(), Some("sess-a"));
    assert_eq!(linked_session(&harness, 6272).as_deref(), Some("sess-b"));
}

#[test]
fn a_task_re_linking_to_its_own_session_is_not_blocked_by_itself() {
    let harness = engine();
    harness.state().task_sessions.insert(
        6270,
        TaskLink {
            cwd: "/repo/one".to_string(),
            session_id: "sess-a".to_string(),
            ..TaskLink::default()
        },
    );
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(&harness, vec![live("sess-a", Some(6270))]);
    assert_eq!(linked_session(&harness, 6270).as_deref(), Some("sess-a"));
}

#[test]
fn a_session_whose_transcript_names_another_task_is_never_taken() {
    let harness = engine();
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(&harness, vec![live("sess-other", Some(9999))]);
    assert_eq!(linked_session(&harness, 6270), None);
    assert_eq!(harness.state().pending.len(), 1, "gave up on the launch");
}

// --- the queue holds every launch in flight ---------------------------------

#[test]
fn a_launch_that_has_not_resolved_survives_one_that_has() {
    let harness = engine();
    queue(
        &harness,
        &[("/repo/one", Some(6270)), ("/repo/other", Some(6681))],
    );
    link(&harness, vec![live("sess-a", Some(6270))]);
    assert_eq!(linked_session(&harness, 6270).as_deref(), Some("sess-a"));
    assert_eq!(
        harness
            .state()
            .pending
            .iter()
            .map(|launch| launch.task_id)
            .collect::<Vec<_>>(),
        vec![Some(6681)],
        "a resolved launch took an unresolved one with it"
    );
}

#[test]
fn a_re_launch_replaces_its_own_entry_and_no_other() {
    let harness = engine();
    queue(
        &harness,
        &[("/repo/one", Some(6270)), ("/repo/other", Some(6681))],
    );
    harness.engine.set_pending(PendingRequest {
        cwd: "/repo/worktree".to_string(),
        task_id: Some(6270),
        known_session_ids: Vec::new(),
    });
    let state = harness.state();
    assert_eq!(
        state
            .pending
            .iter()
            .map(|launch| launch.task_id)
            .collect::<Vec<_>>(),
        vec![Some(6681), Some(6270)]
    );
    assert_eq!(
        state
            .pending
            .iter()
            .find(|launch| launch.task_id == Some(6270))
            .map(|launch| launch.cwd.as_str()),
        Some("/repo/worktree")
    );
}

#[test]
fn a_launch_older_than_the_fast_refresh_window_is_dropped() {
    let harness = engine();
    let now = SystemTime::now();
    harness.state().pending = vec![PendingLaunch {
        cwd: "/repo/one".to_string(),
        task_id: Some(6270),
        launched_at: now - Duration::from_secs(60),
        known_session_ids: Default::default(),
    }];
    harness.state().prune_pending(now);
    assert!(harness.state().pending.is_empty());
}

#[test]
fn a_session_that_already_existed_is_only_taken_on_the_second_pass() {
    // `--resume` reuses a session id, so a known session is not new to us — but
    // an unknown one in the same folder is preferred.
    let harness = engine();
    harness.state().pending = vec![PendingLaunch {
        cwd: "/repo/one".to_string(),
        task_id: Some(6270),
        launched_at: SystemTime::now(),
        known_session_ids: ["sess-old".to_string()].into_iter().collect(),
    }];
    link(
        &harness,
        vec![live("sess-old", None), live("sess-new", None)],
    );
    assert_eq!(linked_session(&harness, 6270).as_deref(), Some("sess-new"));
}

// --- the 4b contract --------------------------------------------------------

#[test]
fn every_link_reaches_the_index() {
    let harness = engine();
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(&harness, vec![live("sess-a", Some(6270))]);

    let row = harness
        .engine
        .db()
        .task_session(6270)
        .expect("the link never reached the index");
    assert_eq!(row.session_file, "/transcripts/sess-a.jsonl");
    assert_eq!(row.cwd, "/repo/one");
    assert_eq!(
        harness.engine.db().task_for_session_file(&row.session_file),
        Some(row)
    );
}

#[test]
fn a_link_reports_itself_as_running_and_announces_a_wired_session() {
    let harness = engine();
    let events = harness.engine.subscribe();
    queue(&harness, &[("/repo/one", Some(6270))]);
    link(
        &harness,
        vec![Session {
            prompts: vec![crate::types::Prompt {
                text: "secret".to_string(),
                timestamp: "t".to_string(),
            }],
            last_entry: Some(tool_entry("Bash")),
            ..live("sess-a", Some(6270))
        }],
    );

    assert_eq!(
        harness.state().task_sessions[&6270].status,
        Some(TaskLinkStatus::Running)
    );
    let announced = events
        .try_iter()
        .find_map(|event| match event {
            crate::daemon::EngineEvent::SessionLinked(session) => Some(*session),
            _ => None,
        })
        .expect("no session-linked event");
    assert_eq!(announced.session_id, "sess-a");
    assert!(announced.prompts.is_empty(), "prompts crossed the boundary");
    assert!(announced.last_entry.is_none());
}
