//! A session that stopped on an API error: one notification, cleared when it
//! moves on.

use super::*;
use crate::daemon::EngineEvent;
use crate::types::{ApiError, LastEntry, NotificationLevel};

fn stopped(id: &str, at: &str, text: &str) -> Session {
    Session {
        task_id: Some(6391),
        last_entry: Some(LastEntry {
            activity_at: Some(at.to_string()),
            api_error: Some(ApiError {
                kind: "server_error".to_string(),
                text: text.to_string(),
            }),
            ..LastEntry::default()
        }),
        ..a_session(id, "/repo/aurora")
    }
}

fn rows(harness: &TestEngine) -> Vec<crate::types::Notification> {
    harness
        .state()
        .notifications
        .iter()
        .filter(|n| n.id.starts_with("apierr-"))
        .cloned()
        .collect()
}

#[test]
fn a_stopped_session_rings_once_and_says_why() {
    let harness = engine();
    let events = harness.engine.subscribe();
    let sessions = index(vec![stopped(
        "s1",
        "2026-09-30T12:00:05Z",
        "Request timed out",
    )]);
    for _ in 0..3 {
        harness.inner().notify_api_errors(&sessions, true);
    }
    let rows = rows(&harness);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].level, NotificationLevel::Error);
    assert_eq!(rows[0].task_id, Some(6391));
    assert!(
        rows[0].title.contains("stopped on task #6391"),
        "{}",
        rows[0].title
    );
    assert!(
        rows[0].message.starts_with("Request timed out"),
        "{}",
        rows[0].message
    );
    let rang = events
        .try_iter()
        .filter(|event| matches!(event, EngineEvent::Notification(_)))
        .count();
    assert_eq!(rang, 1);
}

/// A second failure on the same session replaces the row rather than adding
/// one.
#[test]
fn a_second_failure_replaces_the_row() {
    let harness = engine();
    harness.inner().notify_api_errors(
        &index(vec![stopped(
            "s1",
            "2026-09-30T12:00:05Z",
            "Request timed out",
        )]),
        true,
    );
    harness.inner().notify_api_errors(
        &index(vec![stopped(
            "s1",
            "2026-09-30T12:09:00Z",
            "529 Overloaded",
        )]),
        true,
    );
    let rows = rows(&harness);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].message.starts_with("529 Overloaded"));
}

#[test]
fn the_row_goes_when_the_session_moves_on_or_goes_away() {
    let harness = engine();
    harness.inner().notify_api_errors(
        &index(vec![
            stopped("s1", "2026-09-30T12:00:05Z", "Request timed out"),
            stopped("s2", "2026-09-30T12:00:05Z", "Request timed out"),
        ]),
        true,
    );
    assert_eq!(rows(&harness).len(), 2);

    // s1 was told to go on; s2 was killed. An incomplete scan ends nothing.
    let moved_on = a_session("s1", "/repo/aurora");
    harness
        .inner()
        .notify_api_errors(&index(vec![moved_on.clone()]), false);
    assert_eq!(rows(&harness).len(), 1, "an incomplete scan cleared s2");
    harness
        .inner()
        .notify_api_errors(&index(vec![moved_on]), true);
    assert!(rows(&harness).is_empty());
}

/// After a restart the row comes back from SQLite. It is adopted, not rung
/// again.
#[test]
fn a_restored_row_is_adopted_after_a_restart() {
    let first = engine();
    let sessions = index(vec![stopped(
        "s1",
        "2026-09-30T12:00:05Z",
        "Request timed out",
    )]);
    first.inner().notify_api_errors(&sessions, true);
    let restored = first.engine.db().recent_notifications(10);

    let second = engine();
    second.state().notifications = restored.into_iter().collect();
    let events = second.engine.subscribe();
    second.inner().notify_api_errors(&sessions, true);
    assert!(!events
        .try_iter()
        .any(|event| matches!(event, EngineEvent::Notification(_))));
    assert_eq!(rows(&second).len(), 1);
}
