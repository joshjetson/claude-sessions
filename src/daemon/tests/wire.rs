//! What crosses the process boundary, and the feed that survives a restart.
//!
//! The heavy-field stripping is ported from the `payload trimming` block of
//! `test/daemon.test.js`; Phase 6 serialises exactly what these tests inspect.

use std::time::Duration;

use super::*;
use crate::daemon::{wire_session, NewNotification, NOTIFICATION_LIMIT};
use crate::types::{
    CumulativeUsage, Notification, NotificationLevel, NotificationStatus, Prompt, Usage,
};

fn heavy() -> Session {
    Session {
        last_entry: Some(tool_entry("Bash")),
        prompts: vec![Prompt {
            text: "a".to_string(),
            timestamp: "t".to_string(),
        }],
        cumulative_usage: Some(CumulativeUsage {
            input_tokens: 1,
            ..CumulativeUsage::default()
        }),
        last_usage: Some(Usage {
            input_tokens: Some(5),
            ..Usage::default()
        }),
        git_branch: Some("main".to_string()),
        ..a_session("s1", "/x")
    }
}

#[test]
fn wire_session_drops_the_heavy_transcript_fields() {
    let wired = wire_session(&heavy());
    assert!(wired.last_entry.is_none());
    assert!(wired.prompts.is_empty());
    assert!(wired.cumulative_usage.is_none());

    // …and everything the UI renders survives.
    assert_eq!(wired.session_id, "s1");
    assert_eq!(wired.cwd, "/x");
    assert_eq!(wired.status, SessionStatus::Working);
    assert_eq!(wired.last_usage.and_then(|u| u.input_tokens), Some(5));
    assert_eq!(wired.git_branch.as_deref(), Some("main"));
    assert!(wired.session_file.is_some());
}

#[test]
fn the_heavy_fields_are_absent_from_the_serialised_form_entirely() {
    // Not merely empty: the keys must not be on the wire at all, or a client
    // reading `lastEntry` gets a null it has to special-case.
    let json = serde_json::to_string(&wire_session(&heavy())).unwrap();
    for key in ["lastEntry", "prompts", "cumulativeUsage"] {
        assert!(!json.contains(key), "{key} crossed the boundary: {json}");
    }
    for key in ["sessionId", "cwd", "status", "lastUsage", "gitBranch"] {
        assert!(json.contains(key), "wire_session dropped {key}");
    }
}

#[test]
fn a_snapshot_carries_wired_sessions_only() {
    let harness = engine();
    harness.state().sessions = index(vec![heavy()]);
    let json = serde_json::to_string(&harness.engine.snapshot()).unwrap();
    assert!(!json.contains("lastEntry"), "{json}");
    assert!(!json.contains("\"prompts\""), "{json}");
    assert!(json.contains("\"s1\""));
}

// --- the feed ---------------------------------------------------------------

fn info(title: &str) -> NewNotification {
    NewNotification::new("test", title, "body")
}

#[test]
fn the_feed_is_newest_first_and_capped() {
    let harness = engine();
    for n in 0..NOTIFICATION_LIMIT + 5 {
        harness.inner().push_notification(info(&format!("n{n}")));
    }
    let state = harness.state();
    assert_eq!(state.notifications.len(), NOTIFICATION_LIMIT);
    assert_eq!(state.notifications[0].title, "n204");
    assert_eq!(state.notifications[NOTIFICATION_LIMIT - 1].title, "n5");
}

#[test]
fn a_notification_carries_its_project_and_reaches_subscribers_and_the_store() {
    let harness = engine();
    let events = harness.engine.subscribe();
    let raised = harness.inner().push_notification(NewNotification {
        cwd: "/Users/someone/dev/repo".to_string(),
        level: NotificationLevel::Warn,
        task_id: Some(7),
        ..info("heads up")
    });

    assert_eq!(raised.project, "someone/repo");
    assert_eq!(raised.status, NotificationStatus::Unread);
    assert!(events.try_iter().any(|event| matches!(
        event,
        crate::daemon::EngineEvent::Notification(n) if n.id == raised.id
    )));
    assert_eq!(
        harness
            .engine
            .db()
            .recent_notifications(10)
            .first()
            .map(|n| n.id.clone()),
        Some(raised.id)
    );
}

#[test]
fn ids_do_not_collide_when_two_are_raised_in_the_same_millisecond() {
    // Node used a bare `Date.now()` for the completion notification, which two
    // tasks finishing together would collide on.
    let harness = engine();
    let first = harness.inner().push_notification(info("a"));
    let second = harness.inner().push_notification(info("b"));
    assert_ne!(first.id, second.id);
}

#[test]
fn status_changes_reach_memory_the_store_and_the_clients() {
    let harness = engine();
    let raised = harness.inner().push_notification(info("a"));
    let events = harness.engine.subscribe();

    let result = harness.engine.set_notification_status(
        std::slice::from_ref(&raised.id),
        NotificationStatus::Resolved,
    );
    assert!(result.ok);
    assert_eq!(
        harness.state().notifications[0].status,
        NotificationStatus::Resolved
    );
    assert_eq!(
        harness.engine.db().recent_notifications(10)[0].status,
        NotificationStatus::Resolved
    );
    assert!(events.try_iter().any(|event| matches!(
        event,
        crate::daemon::EngineEvent::NotificationsChanged { removed: false, .. }
    )));

    // An empty list is refused rather than treated as "all of them".
    assert!(
        !harness
            .engine
            .set_notification_status(&[], NotificationStatus::Read)
            .ok
    );
}

#[test]
fn dismissing_removes_from_memory_and_the_store() {
    let harness = engine();
    let raised = harness.inner().push_notification(info("a"));
    harness.inner().push_notification(info("b"));

    assert!(
        harness
            .engine
            .dismiss_notifications(std::slice::from_ref(&raised.id))
            .ok
    );
    assert_eq!(harness.state().notifications.len(), 1);
    assert_eq!(harness.engine.db().recent_notifications(10).len(), 1);
    assert!(!harness.engine.dismiss_notifications(&[]).ok);
}

#[test]
fn marking_everything_read_needs_no_ids() {
    let harness = engine();
    harness.inner().push_notification(info("a"));
    harness.inner().push_notification(info("b"));

    assert!(harness.engine.mark_notifications_read(&[]).ok);
    assert!(harness
        .state()
        .notifications
        .iter()
        .all(|n| n.status == NotificationStatus::Read));
    assert!(harness
        .engine
        .db()
        .recent_notifications(10)
        .iter()
        .all(|n| n.status == NotificationStatus::Read));
}

#[test]
fn the_feed_is_restored_newest_first_and_no_deeper_than_the_cap() {
    let harness = engine();
    for n in 0..250 {
        harness.engine.db().put_notification(&Notification {
            id: format!("n{n:03}"),
            title: format!("n{n}"),
            message: String::new(),
            cwd: String::new(),
            project: String::new(),
            session_id: None,
            task_id: None,
            level: NotificationLevel::Info,
            // Distinct, ordered stamps: the store returns them by time.
            ts: format!("2026-09-16T10:{:02}:{:02}.000Z", n / 60, n % 60),
            status: NotificationStatus::Unread,
        });
    }

    harness.inner().restore_notifications();

    let state = harness.state();
    assert_eq!(state.notifications.len(), NOTIFICATION_LIMIT);
    assert_eq!(state.notifications[0].title, "n249");
    assert_eq!(
        state.notifications[NOTIFICATION_LIMIT - 1].title,
        "n50",
        "the oldest 50 should have been left in the store, not loaded"
    );
    // Restoring prunes the table, and 250 rows is well inside what it keeps.
    assert_eq!(harness.engine.db().recent_notifications(1000).len(), 250);
}

#[test]
fn usage_is_carried_through_untouched() {
    // Phase 11 defines the shape; the engine only holds it and hands it on.
    let harness = engine_with(Setup {
        usage: Some(Box::new(
            || serde_json::json!({ "ok": true, "session": 42 }),
        )),
        ..Setup::default()
    });
    let events = harness.engine.subscribe();

    let value = harness.engine.refresh_usage().expect("no usage hook ran");
    assert_eq!(value["session"], 42);
    assert_eq!(harness.engine.snapshot().usage, Some(value));
    assert!(events
        .try_iter()
        .any(|event| matches!(event, crate::daemon::EngineEvent::Usage(Some(_)))));
}

#[test]
fn an_engine_with_no_usage_hook_reports_nothing_rather_than_zero() {
    let harness = engine();
    assert!(harness.engine.refresh_usage().is_none());
    assert!(harness.engine.snapshot().usage.is_none());
}

#[test]
fn a_dropped_subscriber_is_forgotten_rather_than_retried() {
    let harness = engine();
    let first = harness.engine.subscribe();
    let second = harness.engine.subscribe();
    assert_eq!(harness.engine.stats().subscribers, 2);

    drop(second);
    harness.inner().push_notification(info("a"));
    assert_eq!(harness.engine.stats().subscribers, 1);
    assert_eq!(first.try_iter().count(), 1);
}

#[test]
fn the_board_filter_is_stored_and_reported() {
    let harness = engine();
    assert_eq!(harness.engine.snapshot().board_filter, "mine");
    harness
        .engine
        .set_board_filter(crate::daemon::BoardFilter::All);
    assert_eq!(harness.engine.snapshot().board_filter, "all");
    assert_eq!(crate::daemon::BoardFilter::from_label("nonsense"), None);
}

#[test]
fn a_task_lookup_is_a_map_hit_not_a_walk_of_the_board() {
    // Mandate #10: `findTaskById` was a nested scan over projects and stages,
    // run on every completion and every stall alert.
    let harness = engine();
    let mut board = crate::types::Board::default();
    let mut project = crate::types::BoardProject {
        project_id: 11,
        ..crate::types::BoardProject::default()
    };
    project.stages.insert(
        "In Progress".to_string(),
        crate::types::BoardStage {
            stage_id: 3,
            sequence: 1,
            tasks: vec![Task {
                subtasks: vec![a_task(2, "In Progress")],
                ..a_task(1, "In Progress")
            }],
        },
    );
    board.projects.insert("Project A".to_string(), project);

    harness.state().set_board(Some(board));
    assert_eq!(harness.state().task(1).map(|task| task.id), Some(1));
    assert_eq!(
        harness.state().task(2).map(|task| task.id),
        Some(2),
        "subtasks must be indexed too"
    );
    assert!(harness.state().task(3).is_none());

    // Replacing the board replaces the index with it.
    harness.state().set_board(None);
    assert!(harness.state().task(1).is_none());
}

#[test]
fn sessions_are_grouped_by_project_and_ordered_newest_activity_first() {
    let now = std::time::SystemTime::now();
    let older = Session {
        session_mtime: now - Duration::from_secs(600),
        ..a_session("old", "/Users/someone/dev/repo")
    };
    let newer = Session {
        session_mtime: now - Duration::from_secs(600),
        // A transcript timestamp beats the file's mtime where it has one.
        last_timestamp: Some(
            chrono::DateTime::<chrono::Utc>::from(now)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        ..a_session("new", "/Users/someone/dev/repo")
    };
    let elsewhere = a_session("other", "/Users/someone/dev/second");

    let built = index(vec![older, newer, elsewhere]);
    assert_eq!(built.project_count(), 2);
    let rows = &built.by_project()["someone/repo"];
    assert_eq!(
        rows.iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["new", "old"]
    );
    assert_eq!(
        built.get("other").map(|s| s.cwd.as_str()),
        Some("/Users/someone/dev/second")
    );
    assert!(built.get("missing").is_none());
}

#[test]
fn an_event_serialises_as_its_wire_name_and_camel_case_payload() {
    // The names are the Node `EVENTS` map, and Phase 6 turns each into an SSE
    // frame — a client written against either implementation reads the same
    // stream.
    let event = crate::daemon::EngineEvent::TaskDone {
        task_id: 7,
        name: "Widget".to_string(),
        project: "Project A".to_string(),
        mr_url: Some("https://git.example.com/mr/1".to_string()),
    };
    assert_eq!(event.name(), "task-done");
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "task-done");
    assert_eq!(json["data"]["taskId"], 7);
    assert_eq!(json["data"]["mrUrl"], "https://git.example.com/mr/1");

    for (event, name) in [
        (
            crate::daemon::EngineEvent::Sessions(Box::default()),
            "sessions",
        ),
        (
            crate::daemon::EngineEvent::NotificationsChanged {
                ids: Vec::new(),
                status: None,
                removed: true,
            },
            "notifications-changed",
        ),
        (crate::daemon::EngineEvent::Usage(None), "usage"),
        (
            crate::daemon::EngineEvent::DeployOutput {
                project: "p".to_string(),
                line: "l".to_string(),
            },
            "deploy-output",
        ),
    ] {
        assert_eq!(event.name(), name);
        assert_eq!(serde_json::to_value(&event).unwrap()["event"], name);
    }
}
