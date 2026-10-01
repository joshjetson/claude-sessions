//! The verdict a QA session parks: one row per task and round, one sound.
//!
//! One task raised four "REVISION REQUIRED" rows in an afternoon, each with
//! its own sound, because the session posted again every time it revised its
//! note. The reviewer needed one thing: this round has a verdict.

use serde_json::json;

use super::server::{post, served};
use super::*;
use crate::daemon::{classify_post, title_task_id, AgentPost, EngineEvent, NewNotification};
use crate::types::{NotificationKind, NotificationLevel, NotificationStatus};

fn agent_post(title: &str, level: NotificationLevel) -> NewNotification {
    NewNotification {
        level,
        ..NewNotification::new("notify", title, "the outcome, then the note's path")
    }
}

/// Archive `rounds` finished rounds for the task, the way QAden does before
/// it starts the next, beside the live round's `run.json`.
fn archive_rounds(harness: &TestEngine, task_id: i64, rounds: u32) {
    let dir = harness.paths.qa_task_dir(task_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run.json"), "{}").unwrap();
    for round in 1..=rounds {
        fs::write(dir.join(format!("run-round{round}.json")), "{}").unwrap();
    }
}

/// How many rang, and how many joined or changed in silence.
fn sounds(events: &std::sync::mpsc::Receiver<EngineEvent>) -> (usize, usize) {
    events
        .try_iter()
        .fold((0, 0), |(rang, silent), event| match event {
            EngineEvent::Notification(_) => (rang + 1, silent),
            EngineEvent::NotificationUpdated(_) => (rang, silent + 1),
            _ => (rang, silent),
        })
}

// --- reading the title -------------------------------------------------------

#[test]
fn a_post_is_classified_by_the_outcome_in_its_title() {
    use NotificationKind::{Info, Verdict};
    let cases = [
        ("QA #6391: PASS", Info, AgentPost::Verdict),
        ("QA #6391: REVISION REQUIRED", Info, AgentPost::Verdict),
        ("QA #6391: revision required", Info, AgentPost::Verdict),
        ("  QA #6391 :  CHECKPOINT", Info, AgentPost::Checkpoint),
        ("QA #6391: blocked on a login", Info, AgentPost::Other),
        ("Deploy finished", Info, AgentPost::Other),
        ("QA 6391: PASS", Info, AgentPost::Other),
        ("anything at all", Verdict, AgentPost::Verdict),
    ];
    for (title, kind, expected) in cases {
        assert_eq!(classify_post(title, kind), expected, "{title:?}");
    }
    assert_eq!(title_task_id("QA #6391: PASS"), Some(6391));
    assert_eq!(title_task_id("QA #x: PASS"), None);
}

// --- one row per round --------------------------------------------------------

#[test]
fn the_first_verdict_of_a_round_rings_and_later_posts_rewrite_it_silently() {
    let harness = engine();
    let events = harness.engine.subscribe();
    let first = harness
        .inner()
        .raise_agent_post(agent_post(
            "QA #6391: REVISION REQUIRED",
            NotificationLevel::Warn,
        ))
        .expect("a verdict is kept");
    assert_eq!(first.id, "verdict-6391-r0");
    assert_eq!(first.kind, NotificationKind::Verdict);
    assert_eq!(first.task_id, Some(6391));

    let mut updated = agent_post("QA #6391: REVISION REQUIRED", NotificationLevel::Warn);
    updated.message = "Updated: one more revision".to_string();
    harness.inner().raise_agent_post(updated);

    assert_eq!(sounds(&events), (1, 1));
    let state = harness.state();
    assert_eq!(state.notifications.len(), 1, "the update added a row");
    assert_eq!(state.notifications[0].message, "Updated: one more revision");
    drop(state);
    let stored = harness.engine.db().recent_notifications(10);
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].message, "Updated: one more revision",
        "SQLite kept the old text"
    );
}

/// A checkpoint needs nobody yet. The verdict that follows it in the same
/// round still rings, in the same row.
#[test]
fn a_checkpoint_is_silent_and_the_verdict_after_it_still_rings() {
    let harness = engine();
    let events = harness.engine.subscribe();
    harness
        .inner()
        .raise_agent_post(agent_post("QA #7001: CHECKPOINT", NotificationLevel::Warn));
    assert_eq!(sounds(&events), (0, 1));

    harness
        .inner()
        .raise_agent_post(agent_post("QA #7001: PASS", NotificationLevel::Success));
    assert_eq!(sounds(&events), (1, 0));
    let state = harness.state();
    assert_eq!(state.notifications.len(), 1);
    assert_eq!(state.notifications[0].title, "QA #7001: PASS");
}

/// A checkpoint after the verdict must not overwrite the one line the
/// reviewer needs. It gets its own silent row.
#[test]
fn a_checkpoint_after_the_verdict_leaves_the_verdict_row_alone() {
    let harness = engine();
    harness.inner().raise_agent_post(agent_post(
        "QA #7002: REVISION REQUIRED",
        NotificationLevel::Warn,
    ));
    let events = harness.engine.subscribe();
    harness
        .inner()
        .raise_agent_post(agent_post("QA #7002: CHECKPOINT", NotificationLevel::Warn));
    assert_eq!(sounds(&events), (0, 1));

    let state = harness.state();
    let title_of = |id: &str| {
        state
            .notifications
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.title.clone())
    };
    assert_eq!(
        title_of("verdict-7002-r0").as_deref(),
        Some("QA #7002: REVISION REQUIRED")
    );
    assert_eq!(
        title_of("checkpoint-7002-r0").as_deref(),
        Some("QA #7002: CHECKPOINT")
    );
}

/// QAden archives a finished round before the next starts. The next round's
/// verdict is news again.
#[test]
fn a_new_round_gets_a_new_row_and_rings_again() {
    let harness = engine();
    archive_rounds(&harness, 7003, 1);
    harness.inner().raise_agent_post(agent_post(
        "QA #7003: REVISION REQUIRED",
        NotificationLevel::Warn,
    ));
    archive_rounds(&harness, 7003, 2);
    let events = harness.engine.subscribe();
    let next = harness
        .inner()
        .raise_agent_post(agent_post("QA #7003: PASS", NotificationLevel::Success))
        .expect("kept");
    assert_eq!(next.id, "verdict-7003-r3");
    assert!(harness
        .state()
        .notifications
        .iter()
        .any(|n| n.id == "verdict-7003-r2"));
    assert_eq!(sounds(&events), (1, 0));
    assert_eq!(harness.state().notifications.len(), 2);
}

/// The ring is remembered in SQLite, so a restart does not ring the same
/// round's verdict again.
#[test]
fn a_round_that_rang_does_not_ring_again_after_a_restart() {
    let harness = engine();
    harness.inner().raise_agent_post(agent_post(
        "QA #7004: REVISION REQUIRED",
        NotificationLevel::Warn,
    ));
    harness.state().notifications.clear();
    let events = harness.engine.subscribe();
    harness.inner().raise_agent_post(agent_post(
        "QA #7004: REVISION REQUIRED",
        NotificationLevel::Warn,
    ));
    assert_eq!(sounds(&events), (0, 1));
}

/// PASS is posted at success level, which the QA role drops for ordinary
/// chatter. A verdict is not chatter: it is kept and it rings.
#[test]
fn the_qa_role_keeps_a_pass() {
    let harness = engine_with(Setup {
        config: Some(json!({ "role": "qa" })),
        ..Setup::default()
    });
    let kept = harness
        .inner()
        .raise_agent_post(agent_post("QA #7005: PASS", NotificationLevel::Success));
    assert!(kept.is_some(), "the QA role dropped a PASS");
    assert_eq!(
        harness.state().notifications[0].status,
        NotificationStatus::Unread
    );
}

/// Anything that is not a verdict or a checkpoint goes through the role
/// policy exactly as before.
#[test]
fn other_posts_are_unchanged() {
    let harness = engine();
    let first = harness
        .inner()
        .raise_agent_post(agent_post("progress", NotificationLevel::Info))
        .expect("dev keeps everything");
    let second = harness
        .inner()
        .raise_agent_post(agent_post("progress", NotificationLevel::Info))
        .expect("dev keeps everything");
    assert!(first.id.starts_with("notify-"));
    assert_ne!(first.id, second.id);
    assert_eq!(harness.state().notifications.len(), 2);
}

/// `POST /notify` is where every verdict arrives from.
#[test]
fn the_notify_route_shares_the_verdict_row() {
    let served = served();
    for message in ["first", "Updated: second"] {
        let answer = post(
            served.port(),
            "/notify",
            &json!({
                "title": "QA #7006: REVISION REQUIRED",
                "message": message,
                "level": "warn",
                "taskId": 7006,
            })
            .to_string(),
        );
        assert_eq!(answer.body["id"], json!("verdict-7006-r0"));
    }
    let notifications = served.engine.snapshot().notifications;
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].message, "Updated: second");
}

// --- the outcome the queue estimates from --------------------------------------

/// A live session for the task, started 30 minutes ago, with some tokens.
fn working_on(task_id: i64) -> Session {
    let started = chrono::Local::now() - chrono::Duration::minutes(30);
    Session {
        task_id: Some(task_id),
        lstart: Some(started.format("%a %b %e %H:%M:%S %Y").to_string()),
        cumulative_usage: Some(crate::types::CumulativeUsage {
            input_tokens: 1_000,
            cache_creation_input_tokens: 100_000,
            cache_read_input_tokens: 9_000_000,
            output_tokens: 40_000,
        }),
        ..a_session("qa-sess", "/repo/aurora")
    }
}

fn outcomes(harness: &TestEngine) -> Vec<crate::qarun::outcomes::QaOutcome> {
    crate::qarun::outcomes::OutcomeLog::new(&harness.paths.runtime_dir).read()
}

/// Once per round, at its first verdict: the time since the session started,
/// and its new tokens, without cache reads.
#[test]
fn the_first_verdict_of_a_round_records_the_pass() {
    let harness = engine();
    harness.state().sessions = index(vec![working_on(7010)]);
    harness.state().auto_qa.tasks = vec![Task {
        project_name: "Aurora".to_string(),
        ..a_task(7010, "QA")
    }];
    harness
        .inner()
        .raise_agent_post(agent_post("QA #7010: PASS", NotificationLevel::Success));
    harness
        .inner()
        .raise_agent_post(agent_post("QA #7010: PASS", NotificationLevel::Success));

    let recorded = outcomes(&harness);
    assert_eq!(recorded.len(), 1, "the update recorded a second pass");
    let pass = &recorded[0];
    assert_eq!(pass.task_id, 7010);
    assert_eq!(pass.project, "Aurora");
    assert_eq!(pass.verdict, "pass");
    assert_eq!(pass.tokens, 141_000);
    assert!((29.0..32.0).contains(&pass.minutes), "{}", pass.minutes);
}

/// With no live session to time, nothing is recorded.
#[test]
fn a_verdict_with_no_session_to_time_records_nothing() {
    let harness = engine();
    harness.inner().raise_agent_post(agent_post(
        "QA #7011: REVISION REQUIRED",
        NotificationLevel::Warn,
    ));
    assert!(outcomes(&harness).is_empty());
}
