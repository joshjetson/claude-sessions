//! Ported from `test/awaiting.test.js` — the notification that fires when a
//! session is blocked on YOU.
//!
//! The original check only recognised `AskUserQuestion` and `ExitPlanMode`. A
//! permission prompt is neither — it is an ordinary tool call whose result
//! never arrives — so a session stopped on one made no sound at all and waited
//! for the fifteen-minute stall sweep to notice. The trade is false positives
//! on slow tools, so the dwell time is what these tests pin down: long enough
//! for a test run or a browser step, far short of fifteen minutes.

use std::time::{Duration, SystemTime};

use super::*;
use crate::daemon::{is_awaiting_user_decision, is_blocked_on_tool_call};
use crate::types::{EntryKind, NotificationLevel};

fn quiet(ms: u64, status: SessionStatus) -> (Session, SystemTime) {
    let now = SystemTime::now();
    let session = Session {
        session_mtime: now - Duration::from_millis(ms),
        status,
        ..a_session("s", "/repo")
    };
    (session, now)
}

#[test]
fn a_pending_tool_call_quiet_for_two_minutes_counts_as_blocked() {
    let (session, now) = quiet(120_000, SessionStatus::Awaiting);
    assert!(is_blocked_on_tool_call(&session, now));
}

#[test]
fn a_slow_tool_still_running_is_not_blocked() {
    // A test run or a browser step legitimately takes a while. Notifying here
    // would train the user to ignore the sound.
    let (session, now) = quiet(60_000, SessionStatus::Awaiting);
    assert!(!is_blocked_on_tool_call(&session, now));
}

#[test]
fn only_the_awaiting_status_qualifies() {
    for status in [
        SessionStatus::Working,
        SessionStatus::Idle,
        SessionStatus::AwaitingInput,
        SessionStatus::Compacting,
        SessionStatus::Starting,
    ] {
        let (session, now) = quiet(600_000, status);
        assert!(
            !is_blocked_on_tool_call(&session, now),
            "{status:?} was reported as blocked"
        );
    }
}

#[test]
fn a_session_with_no_mtime_is_never_reported_rather_than_guessed_at() {
    let session = no_mtime(Session {
        status: SessionStatus::Awaiting,
        ..a_session("s", "/repo")
    });
    assert!(!is_blocked_on_tool_call(&session, SystemTime::now()));
}

#[test]
fn a_transcript_stamped_in_the_future_does_not_read_as_blocked() {
    // Clock skew on a network mount, or a copied transcript.
    let now = SystemTime::now();
    let session = Session {
        session_mtime: now + Duration::from_secs(600),
        status: SessionStatus::Awaiting,
        ..a_session("s", "/repo")
    };
    assert!(!is_blocked_on_tool_call(&session, now));
}

#[test]
fn only_a_question_or_a_plan_counts_as_awaiting_a_decision() {
    assert!(is_awaiting_user_decision(Some(&tool_entry(
        "AskUserQuestion"
    ))));
    assert!(is_awaiting_user_decision(Some(&tool_entry("ExitPlanMode"))));
    assert!(!is_awaiting_user_decision(Some(&tool_entry("Bash"))));
    assert!(!is_awaiting_user_decision(None));
    // A user entry is not the agent asking anything, whatever it contains.
    assert!(!is_awaiting_user_decision(Some(&LastEntry::of_kind(
        EntryKind::User
    ))));
    // Nor is an assistant entry that ended in prose.
    assert!(!is_awaiting_user_decision(Some(&LastEntry {
        kind: EntryKind::Assistant,
        has_message: true,
        ..LastEntry::default()
    })));
}

#[test]
fn a_malformed_session_does_not_panic() {
    let session = Session {
        status: SessionStatus::Awaiting,
        session_file: None,
        ..a_session("", "")
    };
    assert!(!is_blocked_on_tool_call(
        &no_mtime(session),
        SystemTime::now()
    ));
}

// --- through the real engine ------------------------------------------------

#[test]
fn a_question_is_announced_once_and_cleared_when_it_is_answered() {
    let harness = engine();
    let now = SystemTime::now();
    let asking = index(vec![Session {
        last_entry: Some(tool_entry("AskUserQuestion")),
        ..a_session("ask-sess", "/repo/x")
    }]);

    harness.inner().notify_awaiting_decisions(&asking, now);
    {
        let state = harness.state();
        assert_eq!(state.notifications.len(), 1);
        assert!(
            state.notifications[0].title.contains("needs your decision"),
            "{}",
            state.notifications[0].title
        );
        assert_eq!(state.notifications[0].level, NotificationLevel::Warn);
        assert!(state.await_notified.contains("ask-sess"));
    }

    // Still asking on the next tick: edge-triggered, so nothing new.
    harness.inner().notify_awaiting_decisions(&asking, now);
    assert_eq!(harness.state().notifications.len(), 1);

    // Answered: the flag clears so the next question is announced afresh.
    let answered = index(vec![a_session("ask-sess", "/repo/x")]);
    harness.inner().notify_awaiting_decisions(&answered, now);
    assert!(!harness.state().await_notified.contains("ask-sess"));

    harness.inner().notify_awaiting_decisions(&asking, now);
    assert_eq!(harness.state().notifications.len(), 2);
}

#[test]
fn a_permission_prompt_is_announced_as_a_maybe_rather_than_a_question() {
    let harness = engine();
    let now = SystemTime::now();
    // An ordinary tool call whose result never came: status awaiting, quiet for
    // longer than the dwell.
    let stuck = index(vec![Session {
        status: SessionStatus::Awaiting,
        session_mtime: now - Duration::from_secs(150),
        last_entry: Some(tool_entry("Bash")),
        ..a_session("perm-sess", "/repo/x")
    }]);

    harness.inner().notify_awaiting_decisions(&stuck, now);
    let state = harness.state();
    assert_eq!(state.notifications.len(), 1);
    assert!(
        state.notifications[0]
            .title
            .contains("may be waiting on a prompt"),
        "{}",
        state.notifications[0].title
    );
    assert!(state.notifications[0].message.contains("permission prompt"));
}
