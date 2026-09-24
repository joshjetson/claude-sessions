//! Ported from `test/awaiting.test.js` — the notification that fires when a
//! session is blocked on YOU.
//!
//! The original check only recognised `AskUserQuestion` and `ExitPlanMode`. A
//! permission prompt is neither — it is an ordinary tool call whose result
//! never arrives — so a session stopped on one made no sound at all and waited
//! for the fifteen-minute stall sweep to notice.
//!
//! Two sources now say "blocked on a prompt". With the hooks installed, a
//! `PermissionRequest` makes the fused status `awaiting`, and that counts at
//! once. Without hooks, a pending ordinary tool call quiet for the dwell time
//! counts, and the dwell is what the transcript-only tests pin down: long
//! enough for a test run or a browser step, far short of fifteen minutes.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use super::*;
use crate::daemon::{is_awaiting_user_decision, is_blocked_on_tool_call};
use crate::types::{EntryKind, NotificationLevel};

/// A session whose newest conversational line is a pending `tool` call, made
/// `ms` milliseconds ago, with the status the fusion gave it.
fn pending(tool: &str, ms: u64, status: SessionStatus) -> (Session, SystemTime) {
    let now = SystemTime::now();
    let session = Session {
        session_mtime: now - Duration::from_millis(ms),
        status,
        last_entry: Some(tool_entry(tool)),
        ..a_session("s", "/repo")
    };
    (session, now)
}

const NO_HOOKS: bool = false;
const HOOKED: bool = true;

#[test]
fn without_hooks_a_tool_call_quiet_for_two_minutes_counts_as_blocked() {
    let (session, now) = pending("Bash", 120_000, SessionStatus::Working);
    assert!(is_blocked_on_tool_call(&session, NO_HOOKS, now));
}

#[test]
fn without_hooks_a_slow_tool_still_running_is_not_blocked() {
    // A test run or a browser step legitimately takes a while. Notifying here
    // would train the user to ignore the sound.
    let (session, now) = pending("Bash", 60_000, SessionStatus::Working);
    assert!(!is_blocked_on_tool_call(&session, NO_HOOKS, now));
}

#[test]
fn the_dwell_is_measured_from_the_conversation_not_from_the_mtime() {
    // Hook results and other bookkeeping touch the mtime right after every
    // tool call. The tool call itself was three minutes ago.
    let now = SystemTime::now();
    let mut entry = tool_entry("Bash");
    entry.activity_at = Some(
        chrono::DateTime::<chrono::Utc>::from(now - Duration::from_secs(180))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    );
    let session = Session {
        session_mtime: now - Duration::from_secs(1),
        status: SessionStatus::Working,
        last_entry: Some(entry),
        ..a_session("s", "/repo")
    };
    assert!(is_blocked_on_tool_call(&session, NO_HOOKS, now));
}

#[test]
fn a_hook_reported_prompt_counts_at_once() {
    // `awaiting` that is not a question can only come from a
    // PermissionRequest or a permission_prompt notification.
    let (session, now) = pending("Bash", 1_000, SessionStatus::Awaiting);
    assert!(is_blocked_on_tool_call(&session, HOOKED, now));
}

#[test]
fn with_hooks_a_long_running_tool_is_not_guessed_at() {
    // No hook said "prompt", so the tool is running. The transcript-only
    // guess would be a false alarm.
    let (session, now) = pending("Bash", 600_000, SessionStatus::Working);
    assert!(!is_blocked_on_tool_call(&session, HOOKED, now));
}

#[test]
fn a_question_is_never_reported_as_a_prompt() {
    // The question has its own notification, raised by the caller.
    let (session, now) = pending("AskUserQuestion", 600_000, SessionStatus::Awaiting);
    assert!(!is_blocked_on_tool_call(&session, NO_HOOKS, now));
    assert!(!is_blocked_on_tool_call(&session, HOOKED, now));
}

#[test]
fn only_a_working_or_awaiting_session_on_a_tool_call_qualifies() {
    for status in [
        SessionStatus::Idle,
        SessionStatus::AwaitingInput,
        SessionStatus::Compacting,
        SessionStatus::Starting,
    ] {
        let (session, now) = pending("Bash", 600_000, status);
        assert!(
            !is_blocked_on_tool_call(&session, NO_HOOKS, now),
            "{status:?} was reported as blocked"
        );
    }
    // Working, but the newest line is prose or a user line, not a tool call.
    let now = SystemTime::now();
    for entry in [
        LastEntry {
            kind: EntryKind::Assistant,
            has_message: true,
            ..LastEntry::default()
        },
        LastEntry::of_kind(EntryKind::User),
    ] {
        let session = Session {
            session_mtime: now - Duration::from_secs(600),
            status: SessionStatus::Working,
            last_entry: Some(entry),
            ..a_session("s", "/repo")
        };
        assert!(!is_blocked_on_tool_call(&session, NO_HOOKS, now));
    }
}

#[test]
fn a_session_with_no_mtime_is_never_reported_rather_than_guessed_at() {
    let session = no_mtime(Session {
        status: SessionStatus::Working,
        last_entry: Some(tool_entry("Bash")),
        ..a_session("s", "/repo")
    });
    assert!(!is_blocked_on_tool_call(
        &session,
        NO_HOOKS,
        SystemTime::now()
    ));
}

#[test]
fn a_transcript_stamped_in_the_future_does_not_read_as_blocked() {
    // Clock skew on a network mount, or a copied transcript.
    let now = SystemTime::now();
    let session = Session {
        session_mtime: now + Duration::from_secs(600),
        status: SessionStatus::Working,
        last_entry: Some(tool_entry("Bash")),
        ..a_session("s", "/repo")
    };
    assert!(!is_blocked_on_tool_call(&session, NO_HOOKS, now));
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
        status: SessionStatus::Working,
        session_file: None,
        ..a_session("", "")
    };
    assert!(!is_blocked_on_tool_call(
        &no_mtime(session),
        NO_HOOKS,
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

    harness
        .inner()
        .notify_awaiting_decisions(&asking, &HashSet::new(), now);
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
    harness
        .inner()
        .notify_awaiting_decisions(&asking, &HashSet::new(), now);
    assert_eq!(harness.state().notifications.len(), 1);

    // Answered: the flag clears so the next question is announced afresh.
    let answered = index(vec![a_session("ask-sess", "/repo/x")]);
    harness
        .inner()
        .notify_awaiting_decisions(&answered, &HashSet::new(), now);
    assert!(!harness.state().await_notified.contains("ask-sess"));

    harness
        .inner()
        .notify_awaiting_decisions(&asking, &HashSet::new(), now);
    assert_eq!(harness.state().notifications.len(), 2);
}

#[test]
fn a_permission_prompt_is_announced_as_a_maybe_rather_than_a_question() {
    // Without hooks: an ordinary tool call whose result never came, quiet for
    // longer than the dwell. The status machine calls it working, which is
    // what it said before this notification could ever fire.
    let harness = engine();
    let now = SystemTime::now();
    let stuck = index(vec![Session {
        status: SessionStatus::Working,
        session_mtime: now - Duration::from_secs(150),
        last_entry: Some(tool_entry("Bash")),
        ..a_session("perm-sess", "/repo/x")
    }]);

    harness
        .inner()
        .notify_awaiting_decisions(&stuck, &HashSet::new(), now);
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

#[test]
fn a_hook_reported_prompt_is_announced_at_once_and_cleared_when_it_ends() {
    let harness = engine();
    let now = SystemTime::now();
    let hooked: HashSet<String> = ["hook-sess".to_string()].into();
    // The fused status: a PermissionRequest hook one second ago.
    let prompting = index(vec![Session {
        status: SessionStatus::Awaiting,
        session_mtime: now - Duration::from_secs(1),
        last_entry: Some(tool_entry("Bash")),
        ..a_session("hook-sess", "/repo/x")
    }]);

    harness
        .inner()
        .notify_awaiting_decisions(&prompting, &hooked, now);
    {
        let state = harness.state();
        assert_eq!(state.notifications.len(), 1);
        assert!(state.notifications[0]
            .title
            .contains("may be waiting on a prompt"));
        assert!(state.await_notified.contains("hook-sess"));
    }

    // Approved: PostToolUse makes it working, and the flag clears.
    let running = index(vec![Session {
        status: SessionStatus::Working,
        session_mtime: now - Duration::from_secs(600),
        last_entry: Some(tool_entry("Bash")),
        ..a_session("hook-sess", "/repo/x")
    }]);
    harness
        .inner()
        .notify_awaiting_decisions(&running, &hooked, now);
    let state = harness.state();
    assert_eq!(state.notifications.len(), 1);
    assert!(!state.await_notified.contains("hook-sess"));
}

// --- a numbered prompt is an answerable question -----------------------------

#[test]
fn a_question_the_agent_asked_names_its_task_and_is_answerable() {
    // AskUserQuestion is a deliberate hand-back: the agent stopped and wants an
    // answer. Raised as `info` with no task id — which it was — the dashboard
    // could not answer it in the reviewer's name and a run could not count it
    // as blocked, so every numbered prompt meant finding the pane by hand.
    let session = Session {
        task_id: Some(6660),
        last_entry: Some(crate::daemon::tests::tool_entry("AskUserQuestion")),
        ..a_session("s", "/repo")
    };
    let notification = crate::daemon::watchers::awaiting_notification(&session, true);

    assert_eq!(notification.kind, crate::types::NotificationKind::Question);
    assert_eq!(notification.task_id, Some(6660));
}

#[test]
fn a_stalled_tool_call_stays_unanswerable() {
    // Most likely a permission prompt: a modal in this tool's own UI, absent
    // from the transcript, clearable only by the person at the keyboard.
    // Marking it answerable would invite the coordinator to try and be refused
    // — which is how one blocker reached a third attempt.
    let session = Session {
        task_id: Some(6660),
        ..a_session("s", "/repo")
    };
    let notification = crate::daemon::watchers::awaiting_notification(&session, false);

    assert_eq!(notification.kind, crate::types::NotificationKind::Info);
    assert!(notification
        .message
        .contains("Nothing can answer one of those remotely"));
}
