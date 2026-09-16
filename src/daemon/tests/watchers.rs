//! The watchers as the engine runs them: a stall becoming a notification, and
//! the first-run rule that records the backlog instead of announcing it.
//!
//! The detection rules themselves are pure and live in `alerts.rs`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::*;
use crate::daemon::{TaskLink, TaskLinkStatus};

/// A task the engine believes is still being worked.
fn running(session_id: &str) -> TaskLink {
    TaskLink {
        cwd: "/repo".to_string(),
        session_id: session_id.to_string(),
        status: Some(TaskLinkStatus::Running),
        ..TaskLink::default()
    }
}
use crate::types::NotificationLevel;

#[test]
fn a_stalled_session_produces_a_real_notification() {
    let harness = engine();
    let now = SystemTime::now();
    harness.state().task_sessions.insert(
        9101,
        TaskLink {
            cwd: "/repo/x".to_string(),
            session_id: "stall-sess".to_string(),
            status: Some(TaskLinkStatus::Running),
            ..TaskLink::default()
        },
    );

    let quiet = |ago: Duration| {
        index(vec![Session {
            session_mtime: now - ago,
            status: SessionStatus::Idle,
            ..a_session("stall-sess", "/repo/x")
        }])
    };

    harness
        .inner()
        .notify_stalled_sessions(&quiet(22 * MINUTE), now);
    {
        let state = harness.state();
        assert_eq!(state.notifications.len(), 1, "no notification was raised");
        let notification = &state.notifications[0];
        assert!(
            notification
                .title
                .contains("Task #9101 quiet for 22 minutes"),
            "{}",
            notification.title
        );
        assert_eq!(notification.level, NotificationLevel::Warn);
        assert_eq!(notification.task_id, Some(9101));
    }

    // Still stalled on the next tick, but inside the reminder window.
    harness
        .inner()
        .notify_stalled_sessions(&quiet(23 * MINUTE), now);
    assert_eq!(
        harness.state().notifications.len(),
        1,
        "it alerted twice in a row"
    );

    // It starts moving again -> the flag clears, so a future stall alerts fresh.
    let working = index(vec![Session {
        session_mtime: now,
        status: SessionStatus::Working,
        ..a_session("stall-sess", "/repo/x")
    }]);
    harness.inner().notify_stalled_sessions(&working, now);
    assert!(
        !harness.state().stall_alerted_at.contains_key(&9101),
        "the stall flag was not cleared when work resumed"
    );
}

#[test]
fn a_healthy_session_raises_nothing() {
    let harness = engine();
    let now = SystemTime::now();
    harness
        .state()
        .task_sessions
        .insert(9102, running("live-sess"));
    let sessions = index(vec![Session {
        session_mtime: now - Duration::from_secs(30),
        status: SessionStatus::Working,
        ..a_session("live-sess", "/repo/y")
    }]);
    harness.inner().notify_stalled_sessions(&sessions, now);
    assert!(harness.state().notifications.is_empty());
}

// --- first run --------------------------------------------------------------

/// The assignment fetch, scripted: a queue the test can add to.
fn queued_fetch(queue: Arc<Mutex<Vec<Task>>>) -> crate::daemon::AssignedFetch {
    Box::new(move |_stages| Ok(queue.lock().unwrap().clone()))
}

#[test]
fn the_existing_backlog_is_recorded_silently_not_announced() {
    // There were thirteen tasks already sitting in "Approved to Start" when
    // this was built. Announcing all of them on first start would be noise.
    let backlog: Vec<Task> = (0..13)
        .map(|index| a_task(7000 + index, "Approved to Start"))
        .collect();
    let queue = Arc::new(Mutex::new(backlog));
    let harness = engine_with(Setup {
        assigned: Some(queued_fetch(Arc::clone(&queue))),
        ..Setup::default()
    });

    harness.inner().notify_new_assignments();
    assert!(
        harness.state().notifications.is_empty(),
        "the whole backlog was announced on first run"
    );
    assert!(harness.engine.db().was_alerted("alerts:bootstrapped"));

    // A genuinely new task afterwards DOES alert.
    queue
        .lock()
        .unwrap()
        .push(a_task(7999, "Approved to Start"));
    harness.inner().notify_new_assignments();
    {
        let state = harness.state();
        assert_eq!(
            state.notifications.len(),
            1,
            "a new arrival was not announced"
        );
        assert!(state.notifications[0].title.contains("#7999"));
    }

    // …and only once.
    harness.inner().notify_new_assignments();
    assert_eq!(
        harness.state().notifications.len(),
        1,
        "the same arrival alerted twice"
    );
}

#[test]
fn an_odoo_failure_is_swallowed_rather_than_killing_the_poll_loop() {
    let harness = engine_with(Setup {
        assigned: Some(Box::new(|_stages| Err("odoo down".to_string()))),
        ..Setup::default()
    });
    harness.inner().notify_new_assignments();
    assert!(harness.state().notifications.is_empty());
}

#[test]
fn alerts_switched_off_in_config_raise_nothing() {
    let queue = Arc::new(Mutex::new(vec![a_task(1, "Approved to Start")]));
    let harness = engine_with(Setup {
        config: Some(serde_json::json!({ "alerts": { "enabled": false } })),
        assigned: Some(queued_fetch(queue)),
        ..Setup::default()
    });
    harness.inner().notify_new_assignments();
    assert!(harness.state().notifications.is_empty());
    assert!(!harness.engine.db().was_alerted("alerts:bootstrapped"));
}
