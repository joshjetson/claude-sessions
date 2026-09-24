//! What the user's role changes on the daemon side: which notifications are
//! kept, the QA arrival watcher, and the one quiet-sessions row. Plus the two
//! feed fixes every role gets: questions resolve when the wait ends, and Clear
//! all resolves the whole feed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::server::{post, served, wait_for};
use super::*;
use crate::daemon::{EngineEvent, NewNotification, RefreshRequest};
use crate::odoo::QaStageTask;
use crate::types::{
    NotificationKind, NotificationLevel, NotificationPolicy, NotificationStatus, UserRole,
    QUIET_SESSIONS_ID,
};

fn qa_engine() -> TestEngine {
    engine_with(Setup {
        config: Some(json!({ "role": "qa" })),
        ..Setup::default()
    })
}

fn raised(
    source: &'static str,
    kind: NotificationKind,
    level: NotificationLevel,
) -> NewNotification {
    NewNotification {
        kind,
        level,
        ..NewNotification::new(source, format!("{source} {kind:?} {level:?}"), "")
    }
}

// --- the policy --------------------------------------------------------------

/// The whole table, one row per source the daemon raises. Dev and PM keep
/// everything. QA keeps what a reviewer acts on.
#[test]
fn the_policy_table_per_role_and_source() {
    use NotificationKind::{Info, Question, Verdict};
    use NotificationLevel::{Error, Success, Warn};
    let cases: &[(&str, NotificationKind, NotificationLevel, bool)] = &[
        ("await", Question, Warn, true),
        ("await", Info, Warn, true),
        ("done", Info, Success, true),
        ("blocked", Info, Warn, true),
        ("qa-new", Info, NotificationLevel::Info, true),
        ("quiet", Info, Warn, true),
        ("notify", Question, NotificationLevel::Info, true),
        ("notify", Verdict, NotificationLevel::Info, true),
        ("notify", Info, Warn, true),
        ("notify", Info, Error, true),
        ("notify", Info, NotificationLevel::Info, false),
        ("notify", Info, Success, false),
        ("assigned", Info, NotificationLevel::Info, false),
        ("stalled", Info, Warn, false),
        // A source this policy does not know about is kept.
        ("error-log", Info, Error, true),
    ];
    for &(source, kind, level, qa_keeps) in cases {
        assert_eq!(
            UserRole::Qa
                .notification_policy()
                .keeps(source, kind, level),
            qa_keeps,
            "QA: {source} {kind:?} {level:?}"
        );
        for role in [UserRole::Dev, UserRole::Pm] {
            assert!(
                role.notification_policy().keeps(source, kind, level),
                "{role:?} dropped {source} {kind:?} {level:?}"
            );
        }
    }
    assert_eq!(
        UserRole::Pm.notification_policy(),
        NotificationPolicy::Everything
    );
}

/// Filtered at push time: a dropped notification is not in the feed, not in
/// SQLite, and never reaches a client, so it cannot ring.
#[test]
fn the_qa_role_drops_at_push_time_and_stores_nothing() {
    let harness = qa_engine();
    let events = harness.engine.subscribe();
    for new in [
        raised("assigned", NotificationKind::Info, NotificationLevel::Info),
        raised("stalled", NotificationKind::Info, NotificationLevel::Warn),
        raised("notify", NotificationKind::Info, NotificationLevel::Success),
    ] {
        assert!(harness.inner().raise_notification(new).is_none());
    }
    let kept = harness
        .inner()
        .raise_notification(raised(
            "notify",
            NotificationKind::Question,
            NotificationLevel::Info,
        ))
        .expect("a question is kept");

    assert_eq!(harness.state().notifications.len(), 1);
    let stored = harness.engine.db().recent_notifications(50);
    assert_eq!(stored.len(), 1, "a dropped notification reached SQLite");
    assert_eq!(stored[0].id, kept.id);
    let sent: Vec<EngineEvent> = events.try_iter().collect();
    assert_eq!(sent.len(), 1, "a dropped notification was published");
}

#[test]
fn the_dev_role_keeps_every_source_unchanged() {
    let harness = engine();
    for source in ["assigned", "stalled", "notify", "await", "done"] {
        assert!(harness
            .inner()
            .raise_notification(raised(
                source,
                NotificationKind::Info,
                NotificationLevel::Info
            ))
            .is_some());
    }
    assert_eq!(harness.state().notifications.len(), 5);
}

/// The role is read each time, from a config the tick re-reads when the file
/// changes. No restart.
#[test]
fn a_role_edited_in_the_file_applies_on_the_next_tick() {
    let harness = engine();
    let chatter = || raised("notify", NotificationKind::Info, NotificationLevel::Info);
    assert!(harness.inner().raise_notification(chatter()).is_some());

    fs::write(
        &harness.paths.config_path,
        json!({ "role": "qa" }).to_string(),
    )
    .unwrap();
    harness.engine.refresh(RefreshRequest::default());
    assert!(
        harness.inner().raise_notification(chatter()).is_none(),
        "the edited role was not picked up"
    );

    fs::write(
        &harness.paths.config_path,
        json!({ "role": "dev" }).to_string(),
    )
    .unwrap();
    harness.engine.refresh(RefreshRequest::default());
    assert!(harness.inner().raise_notification(chatter()).is_some());
}

/// The QA role gets one aggregated row instead of the per-task stall alert.
#[test]
fn the_qa_role_raises_no_per_task_stall_alert() {
    let harness = qa_engine();
    let now = SystemTime::now();
    harness.state().task_sessions.insert(
        9101,
        crate::daemon::TaskLink {
            cwd: "/repo/x".to_string(),
            session_id: "stall-sess".to_string(),
            status: Some(crate::daemon::TaskLinkStatus::Running),
            ..crate::daemon::TaskLink::default()
        },
    );
    let sessions = index(vec![Session {
        session_mtime: now - 40 * MINUTE,
        status: SessionStatus::Idle,
        ..a_session("stall-sess", "/repo/x")
    }]);
    harness.inner().notify_stalled_sessions(&sessions, now);
    assert!(harness.state().notifications.is_empty());
    assert!(harness.state().stall_alerted_at.is_empty());
}

/// `POST /notify` goes through the same policy. A filtered post still answers
/// 200: the agent that sent it did nothing wrong and must not retry.
#[test]
fn a_filtered_post_answers_ok_and_says_it_was_filtered() {
    let served = served();
    fs::write(
        &served.paths.config_path,
        json!({ "role": "qa" }).to_string(),
    )
    .unwrap();
    served.engine.inner().sync_config();

    let chatter = post(
        served.port(),
        "/notify",
        &json!({ "title": "progress", "level": "info" }).to_string(),
    );
    assert_eq!(chatter.status, 200);
    assert_eq!(chatter.body["ok"], json!(true));
    assert_eq!(chatter.body["filtered"], json!(true));
    assert!(chatter.body["id"].is_null());

    let question = post(
        served.port(),
        "/notify",
        &json!({ "title": "which env?", "kind": "question" }).to_string(),
    );
    assert!(question.body["id"].is_string());
    assert_eq!(served.engine.snapshot().notifications.len(), 1);
}

// --- QA arrivals -------------------------------------------------------------

fn entry(id: i64, stage: &str, project: &str) -> QaStageTask {
    QaStageTask {
        task: Task {
            project_name: project.to_string(),
            ..a_task(id, stage)
        },
        user_ids: Vec::new(),
        assigned_to_me: false,
        stage_entered: "2026-09-24 09:00:00".to_string(),
    }
}

/// A scripted Odoo: whatever the queue holds, plus a call counter.
fn scripted(
    queue: Arc<Mutex<Vec<QaStageTask>>>,
    calls: Arc<AtomicUsize>,
) -> crate::daemon::QaStageFetch {
    Box::new(move |_stages| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(queue.lock().unwrap().clone())
    })
}

fn qa_watcher(
    config: serde_json::Value,
    tasks: Vec<QaStageTask>,
) -> (TestEngine, Arc<Mutex<Vec<QaStageTask>>>, Arc<AtomicUsize>) {
    let queue = Arc::new(Mutex::new(tasks));
    let calls = Arc::new(AtomicUsize::new(0));
    let harness = engine_with(Setup {
        config: Some(config),
        qa_stage: Some(scripted(Arc::clone(&queue), Arc::clone(&calls))),
        ..Setup::default()
    });
    (harness, queue, calls)
}

fn titles(harness: &TestEngine) -> Vec<String> {
    harness
        .state()
        .notifications
        .iter()
        .map(|n| n.title.clone())
        .collect()
}

#[test]
fn the_first_run_records_the_qa_backlog_silently_then_announces_arrivals_once() {
    let backlog = (0..12).map(|i| entry(8000 + i, "QA", "Aurora")).collect();
    let (harness, queue, _) = qa_watcher(json!({ "role": "qa" }), backlog);

    harness.inner().notify_qa_arrivals();
    assert!(titles(&harness).is_empty(), "the backlog was announced");
    assert!(harness.engine.db().was_alerted("qa-new:bootstrapped"));

    queue
        .lock()
        .unwrap()
        .push(entry(8100, "Quality Assurance", "Aurora"));
    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness), ["🧪 New in QA: #8100 task 8100"]);
    let raised = harness.state().notifications[0].clone();
    assert_eq!(raised.task_id, Some(8100));
    assert_eq!(raised.project, "Aurora");
    assert!(raised.id.starts_with("qa-new-"));

    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness).len(), 1, "the same arrival rang twice");
}

/// Another reviewer's task is their notification. Assigned to both of you, it
/// is still yours.
#[test]
fn a_task_another_reviewer_has_is_not_announced_unless_it_is_yours_too() {
    let (harness, queue, _) = qa_watcher(
        json!({ "role": "qa", "qa": { "otherQaUserIds": [24] } }),
        Vec::new(),
    );
    harness.inner().notify_qa_arrivals();

    let theirs = QaStageTask {
        user_ids: vec![24],
        ..entry(8201, "QA", "Aurora")
    };
    let shared = QaStageTask {
        user_ids: vec![24, 7],
        assigned_to_me: true,
        ..entry(8202, "QA", "Aurora")
    };
    let dev_only = QaStageTask {
        user_ids: vec![99],
        ..entry(8203, "QA", "Aurora")
    };
    queue.lock().unwrap().extend([theirs, shared, dev_only]);
    harness.inner().notify_qa_arrivals();

    let mut got = titles(&harness);
    got.sort();
    assert_eq!(
        got,
        [
            "🧪 Assigned to you · #8202 task 8202",
            "🧪 New in QA: #8203 task 8203",
        ]
    );
    // Recorded anyway, so it does not ring later.
    assert!(harness
        .engine
        .db()
        .was_alerted("qa-new:8201:qa:2026-09-24 09:00:00"));
}

#[test]
fn revision_stages_never_announce_even_when_listed() {
    let (harness, queue, _) = qa_watcher(
        json!({ "role": "qa", "qa": { "newTaskStages": ["QA", "Revision Required"] } }),
        Vec::new(),
    );
    harness.inner().notify_qa_arrivals();
    queue.lock().unwrap().extend([
        entry(8301, "Revision Required", "Aurora"),
        entry(8302, "QA", "Aurora"),
    ]);
    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness), ["🧪 New in QA: #8302 task 8302"]);
}

#[test]
fn only_your_projects_announce_and_ignored_ones_stay_quiet() {
    let (harness, queue, _) = qa_watcher(
        json!({
            "role": "qa",
            "odooProjectDirs": { "Aurora": "~/dev/aurora", "Legacy": "~/dev/legacy" },
            "board": { "ignore": ["legacy"] }
        }),
        Vec::new(),
    );
    harness.inner().notify_qa_arrivals();
    queue.lock().unwrap().extend([
        entry(8401, "QA", "aurora"),
        entry(8402, "QA", "Somebody Else"),
        entry(8403, "QA", "Legacy"),
    ]);
    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness), ["🧪 New in QA: #8401 task 8401"]);
}

/// QA -> revision -> QA is new work again. Odoo stamps a new stage-entry time,
/// and that is part of the key.
#[test]
fn a_task_back_in_qa_after_a_revision_announces_again() {
    let (harness, queue, _) = qa_watcher(json!({ "role": "qa" }), Vec::new());
    harness.inner().notify_qa_arrivals();
    queue.lock().unwrap().push(entry(8501, "QA", "Aurora"));
    harness.inner().notify_qa_arrivals();

    queue.lock().unwrap()[0].stage_entered = "2026-09-25 14:00:00".to_string();
    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness).len(), 2);
}

/// The watcher costs an Odoo query, so it runs for the QA role only. Coming
/// back to QA records the stages silently instead of replaying what arrived
/// meanwhile.
#[test]
fn other_roles_skip_the_query_and_switching_back_does_not_replay() {
    let (harness, queue, calls) = qa_watcher(json!({ "role": "qa" }), Vec::new());
    harness.inner().notify_qa_arrivals();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    fs::write(
        &harness.paths.config_path,
        json!({ "role": "dev" }).to_string(),
    )
    .unwrap();
    harness.inner().sync_config();
    queue.lock().unwrap().push(entry(8601, "QA", "Aurora"));
    harness.inner().notify_qa_arrivals();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the dev role queried Odoo");

    fs::write(
        &harness.paths.config_path,
        json!({ "role": "qa" }).to_string(),
    )
    .unwrap();
    harness.inner().sync_config();
    harness.inner().notify_qa_arrivals();
    assert!(titles(&harness).is_empty(), "the switch replayed arrivals");

    queue.lock().unwrap().push(entry(8602, "QA", "Aurora"));
    harness.inner().notify_qa_arrivals();
    assert_eq!(titles(&harness), ["🧪 New in QA: #8602 task 8602"]);
}

#[test]
fn an_odoo_failure_in_the_qa_watcher_is_swallowed() {
    let harness = engine_with(Setup {
        config: Some(json!({ "role": "qa" })),
        qa_stage: Some(Box::new(|_stages| Err("odoo down".to_string()))),
        ..Setup::default()
    });
    harness.inner().notify_qa_arrivals();
    assert!(harness.state().notifications.is_empty());
    assert!(!harness.engine.db().was_alerted("qa-new:bootstrapped"));
}

// --- the quiet-sessions row --------------------------------------------------

fn quiet_sessions(count: usize, silent: Duration, now: SystemTime) -> Vec<Session> {
    (0..count)
        .map(|i| Session {
            session_mtime: now - silent,
            status: SessionStatus::Idle,
            ..a_session(&format!("quiet-{i}"), &format!("/repo/project-{i}"))
        })
        .collect()
}

/// How many of the published events would have rung, and how many were
/// silent in-place updates, for the quiet row only.
fn quiet_events(events: &std::sync::mpsc::Receiver<EngineEvent>) -> (usize, usize) {
    let mut rang = 0;
    let mut updated = 0;
    for event in events.try_iter() {
        match event {
            EngineEvent::Notification(n) if n.id == QUIET_SESSIONS_ID => rang += 1,
            EngineEvent::NotificationUpdated(n) if n.id == QUIET_SESSIONS_ID => updated += 1,
            _ => {}
        }
    }
    (rang, updated)
}

fn quiet_rows(harness: &TestEngine) -> usize {
    harness
        .state()
        .notifications
        .iter()
        .filter(|n| n.id == QUIET_SESSIONS_ID)
        .count()
}

/// The acceptance case: ten sessions, the reviewer away for an hour. One row,
/// one sound, however many ticks run.
#[test]
fn ten_quiet_sessions_over_an_hour_make_one_row_and_one_sound() {
    let harness = qa_engine();
    let events = harness.engine.subscribe();
    let start = SystemTime::now();

    for minute in 0..=60u64 {
        let now = start + Duration::from_secs(minute * 60);
        let sessions = index(quiet_sessions(10, Duration::from_secs(minute * 60), now));
        harness.inner().update_quiet_sessions(&sessions, now);
    }

    assert_eq!(quiet_rows(&harness), 1);
    assert_eq!(
        harness.state().notifications.len(),
        1,
        "a second row appeared"
    );
    let (rang, updated) = quiet_events(&events);
    assert_eq!(rang, 1, "the row rang more than once");
    assert!(updated >= 40, "the row was not kept current ({updated})");

    let row = harness.state().notifications[0].clone();
    assert_eq!(
        row.title,
        format!(
            "⏳ 10 sessions quiet 15m+ — longest: {} 1h 00m",
            project_name("/repo/project-0")
        )
    );
    assert_eq!(row.level, NotificationLevel::Warn);
    assert!(row.message.contains("+5 more"), "{}", row.message);
    // Rebuilt from live sessions every tick, so it is never persisted.
    assert!(harness.engine.db().recent_notifications(50).is_empty());
}

#[test]
fn sessions_below_the_threshold_compacting_or_starting_are_not_quiet() {
    let harness = qa_engine();
    let now = SystemTime::now();
    let sessions = index(vec![
        Session {
            session_mtime: now - 5 * MINUTE,
            ..a_session("fresh", "/repo/a")
        },
        Session {
            session_mtime: now - 50 * MINUTE,
            status: SessionStatus::Compacting,
            ..a_session("compacting", "/repo/b")
        },
        Session {
            session_mtime: now - 50 * MINUTE,
            ..a_placeholder(99, "/repo/c")
        },
    ]);
    harness.inner().update_quiet_sessions(&sessions, now);
    assert_eq!(quiet_rows(&harness), 0);
}

#[test]
fn the_row_goes_away_when_nothing_is_quiet() {
    let harness = qa_engine();
    let now = SystemTime::now();
    harness
        .inner()
        .update_quiet_sessions(&index(quiet_sessions(3, 20 * MINUTE, now)), now);
    assert_eq!(quiet_rows(&harness), 1);

    let events = harness.engine.subscribe();
    harness
        .inner()
        .update_quiet_sessions(&index(quiet_sessions(3, Duration::ZERO, now)), now);
    assert_eq!(quiet_rows(&harness), 0);
    assert!(events.try_iter().any(|event| matches!(
        event,
        EngineEvent::NotificationsChanged { ref ids, removed: true, .. }
            if ids == &[QUIET_SESSIONS_ID.to_string()]
    )));
}

/// Dismissed, it stays hidden while the same sessions stay quiet. A session
/// that was not quiet at dismissal re-raises it, once, with a sound.
#[test]
fn a_dismissed_row_stays_hidden_until_a_new_session_goes_quiet() {
    let harness = qa_engine();
    let now = SystemTime::now();
    let three = quiet_sessions(3, 20 * MINUTE, now);
    harness
        .inner()
        .update_quiet_sessions(&index(three.clone()), now);
    harness
        .inner()
        .dismiss_notifications(&[QUIET_SESSIONS_ID.to_string()]);
    assert_eq!(quiet_rows(&harness), 0);

    let events = harness.engine.subscribe();
    for _ in 0..5 {
        harness
            .inner()
            .update_quiet_sessions(&index(three.clone()), now);
    }
    assert_eq!(quiet_rows(&harness), 0, "the dismissed row came back");
    assert_eq!(quiet_events(&events), (0, 0));

    let four = quiet_sessions(4, 20 * MINUTE, now);
    harness.inner().update_quiet_sessions(&index(four), now);
    assert_eq!(quiet_rows(&harness), 1);
    assert_eq!(quiet_events(&events).0, 1);
}

/// Resolving it from the menu is a dismissal too. A session that wrote again
/// and then went quiet again counts as new.
#[test]
fn resolving_the_row_dismisses_it_and_a_fresh_silence_is_news() {
    let harness = qa_engine();
    let now = SystemTime::now();
    let two = quiet_sessions(2, 20 * MINUTE, now);
    harness
        .inner()
        .update_quiet_sessions(&index(two.clone()), now);
    harness.inner().set_notification_status(
        &[QUIET_SESSIONS_ID.to_string()],
        NotificationStatus::Resolved,
    );
    harness
        .inner()
        .update_quiet_sessions(&index(two.clone()), now);
    assert!(harness
        .state()
        .notifications
        .iter()
        .all(|n| n.status == NotificationStatus::Resolved));

    // quiet-0 writes again, then goes quiet again.
    let mut moving = two.clone();
    moving[0].session_mtime = now;
    harness.inner().update_quiet_sessions(&index(moving), now);
    harness.inner().update_quiet_sessions(&index(two), now);
    assert_eq!(
        quiet_rows(&harness),
        1,
        "the resolved copy was not replaced"
    );
    assert_eq!(
        harness.state().notifications[0].status,
        NotificationStatus::Unread
    );
}

#[test]
fn the_dev_role_has_no_quiet_row_and_leaving_qa_removes_it() {
    let dev = engine();
    let now = SystemTime::now();
    dev.inner()
        .update_quiet_sessions(&index(quiet_sessions(10, 60 * MINUTE, now)), now);
    assert_eq!(quiet_rows(&dev), 0);

    let harness = qa_engine();
    harness
        .inner()
        .update_quiet_sessions(&index(quiet_sessions(2, 60 * MINUTE, now)), now);
    assert_eq!(quiet_rows(&harness), 1);
    fs::write(
        &harness.paths.config_path,
        json!({ "role": "dev" }).to_string(),
    )
    .unwrap();
    harness.inner().sync_config();
    harness
        .inner()
        .update_quiet_sessions(&index(quiet_sessions(2, 60 * MINUTE, now)), now);
    assert_eq!(quiet_rows(&harness), 0);
}

// --- questions resolve when the wait ends -------------------------------------

#[test]
fn a_question_resolves_when_the_session_stops_waiting() {
    let harness = engine();
    let now = SystemTime::now();
    let asking = Session {
        last_entry: Some(tool_entry("AskUserQuestion")),
        ..a_session("asker", "/repo/q")
    };
    harness.inner().notify_awaiting_decisions(
        &index(vec![asking.clone()]),
        &Default::default(),
        now,
    );
    let question = harness.state().notifications[0].clone();
    assert_eq!(question.kind, NotificationKind::Question);
    assert_eq!(question.status, NotificationStatus::Unread);

    let events = harness.engine.subscribe();
    let answered = Session {
        last_entry: Some(tool_entry("Bash")),
        status: SessionStatus::Working,
        ..asking
    };
    harness
        .inner()
        .notify_awaiting_decisions(&index(vec![answered]), &Default::default(), now);

    assert_eq!(
        harness.state().notifications[0].status,
        NotificationStatus::Resolved
    );
    assert_eq!(
        harness.engine.db().recent_notifications(10)[0].status,
        NotificationStatus::Resolved,
        "the resolution did not reach SQLite"
    );
    assert!(
        events.try_iter().any(|event| matches!(
            event,
            EngineEvent::NotificationsChanged { ref ids, status: Some(NotificationStatus::Resolved), .. }
                if ids == std::slice::from_ref(&question.id)
        )),
        "clients were not told"
    );
}

/// Only the session whose wait ended, and only its questions.
#[test]
fn another_sessions_question_and_other_kinds_stay_open() {
    let harness = engine();
    let now = SystemTime::now();
    let one = Session {
        last_entry: Some(tool_entry("AskUserQuestion")),
        ..a_session("one", "/repo/a")
    };
    let two = Session {
        last_entry: Some(tool_entry("AskUserQuestion")),
        ..a_session("two", "/repo/b")
    };
    harness.inner().notify_awaiting_decisions(
        &index(vec![one.clone(), two.clone()]),
        &Default::default(),
        now,
    );
    harness.inner().push_notification(NewNotification {
        session_id: Some("one".to_string()),
        kind: NotificationKind::Verdict,
        ..NewNotification::new("notify", "PASS?", "")
    });

    let moved_on = Session {
        last_entry: Some(tool_entry("Bash")),
        ..one
    };
    harness.inner().notify_awaiting_decisions(
        &index(vec![moved_on, two]),
        &Default::default(),
        now,
    );

    let state = harness.state();
    let status_of = |session: &str, kind: NotificationKind| {
        state
            .notifications
            .iter()
            .find(|n| n.session_id.as_deref() == Some(session) && n.kind == kind)
            .map(|n| n.status)
    };
    assert_eq!(
        status_of("one", NotificationKind::Question),
        Some(NotificationStatus::Resolved)
    );
    assert_eq!(
        status_of("two", NotificationKind::Question),
        Some(NotificationStatus::Unread)
    );
    assert_eq!(
        status_of("one", NotificationKind::Verdict),
        Some(NotificationStatus::Unread),
        "a verdict is not answered by the session moving on"
    );
}

// --- Clear all --------------------------------------------------------------

#[test]
fn clear_all_through_the_route_resolves_everything_and_persists() {
    let served = served();
    for title in ["a", "b", "c"] {
        served
            .engine
            .push_notification(NewNotification::new("test", title, ""));
    }
    served.engine.set_notification_status(
        &[served.engine.snapshot().notifications[0].id.clone()],
        NotificationStatus::Read,
    );

    assert!(served.client().clear_notifications());
    assert!(served
        .engine
        .snapshot()
        .notifications
        .iter()
        .all(|n| n.status == NotificationStatus::Resolved));
    assert!(served
        .engine
        .db()
        .recent_notifications(10)
        .iter()
        .all(|n| n.status == NotificationStatus::Resolved));
}

/// Clearing everything also counts as dismissing the quiet row.
#[test]
fn clear_all_dismisses_the_quiet_row_too() {
    let harness = qa_engine();
    let now = SystemTime::now();
    let quiet = quiet_sessions(2, 20 * MINUTE, now);
    harness
        .inner()
        .update_quiet_sessions(&index(quiet.clone()), now);
    harness.inner().resolve_all_notifications();
    let events = harness.engine.subscribe();
    harness.inner().update_quiet_sessions(&index(quiet), now);
    assert_eq!(quiet_events(&events), (0, 0));
}

// --- the dashboard's side of the wire ---------------------------------------

/// A dashboard that connects late gets the backlog, hears changes made
/// elsewhere, and its own Clear all reaches the daemon.
#[test]
fn a_remote_feed_loads_the_backlog_follows_changes_and_clears() {
    use crate::ui::feed::{FeedEvent, SessionFeed};
    use crate::ui::feed_remote::RemoteFeed;

    let served = served();
    let backlog =
        served
            .engine
            .push_notification(NewNotification::new("test", "raised before connect", ""));

    let mut feed = RemoteFeed::connect(served.port());
    let mut seen: Vec<FeedEvent> = Vec::new();
    let mut drain_until = |label: &str, test: &dyn Fn(&FeedEvent) -> bool| {
        wait_for(label, || {
            seen.extend(feed.drain());
            seen.iter().any(test)
        });
    };
    drain_until(
        "the backlog",
        &|event| matches!(event, FeedEvent::Notifications(list) if list.iter().any(|n| n.id == backlog.id)),
    );

    served.engine.set_notification_status(
        std::slice::from_ref(&backlog.id),
        NotificationStatus::Resolved,
    );
    drain_until("the status change", &|event| {
        matches!(
            event,
            FeedEvent::NotificationsChanged { ids, status: Some(NotificationStatus::Resolved), .. }
                if ids.contains(&backlog.id)
        )
    });

    served
        .engine
        .inner()
        .update_quiet_row("⏳ quiet".to_string(), "m".to_string());
    drain_until(
        "the silent update",
        &|event| matches!(event, FeedEvent::NotificationUpdated(n) if n.id == QUIET_SESSIONS_ID),
    );

    let late = served
        .engine
        .push_notification(NewNotification::new("test", "late", ""));
    let feed = RemoteFeed::connect(served.port());
    assert!(feed.clear_notifications());
    wait_for("the daemon to resolve everything", || {
        served
            .engine
            .snapshot()
            .notifications
            .iter()
            .any(|n| n.id == late.id && n.status == NotificationStatus::Resolved)
    });
}
