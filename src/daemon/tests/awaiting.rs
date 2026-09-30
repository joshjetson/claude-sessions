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
use crate::daemon::{is_awaiting_user_decision, is_blocked_on_tool_call, EngineEvent};
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

const DWELL: Duration = crate::daemon::PROMPT_DWELL;
const REPEAT: Duration = crate::daemon::PROMPT_REPEAT;
const SECOND: Duration = Duration::from_secs(1);

/// A session a `PermissionRequest` hook has made `awaiting`.
fn prompting(id: &str, now: SystemTime) -> Session {
    Session {
        status: SessionStatus::Awaiting,
        session_mtime: now - SECOND,
        last_entry: Some(tool_entry("Bash")),
        ..a_session(id, "/repo/x")
    }
}

fn asking(id: &str) -> Session {
    Session {
        last_entry: Some(tool_entry("AskUserQuestion")),
        ..a_session(id, "/repo/x")
    }
}

fn hooked(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

/// The rows that are not resolved, oldest last.
fn open_rows(harness: &TestEngine) -> Vec<crate::types::Notification> {
    harness
        .state()
        .notifications
        .iter()
        .filter(|n| n.status != crate::types::NotificationStatus::Resolved)
        .cloned()
        .collect()
}

#[test]
fn a_question_is_announced_once_and_cleared_when_it_is_answered() {
    let harness = engine();
    let now = SystemTime::now();
    let asking = index(vec![asking("ask-sess")]);

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
        assert!(state.awaits.contains_key("ask-sess"));
    }

    // Still asking on the next tick: one row, nothing new.
    harness
        .inner()
        .notify_awaiting_decisions(&asking, &HashSet::new(), now + REPEAT * 2);
    assert_eq!(harness.state().notifications.len(), 1);

    // Answered: the row resolves, so the next question is announced afresh.
    let answered = index(vec![a_session("ask-sess", "/repo/x")]);
    harness
        .inner()
        .notify_awaiting_decisions(&answered, &HashSet::new(), now);
    assert!(!harness.state().awaits.contains_key("ask-sess"));
    assert!(open_rows(&harness).is_empty());

    harness
        .inner()
        .notify_awaiting_decisions(&asking, &HashSet::new(), now);
    assert_eq!(harness.state().notifications.len(), 2);
    assert_eq!(open_rows(&harness).len(), 1);
}

#[test]
fn a_permission_prompt_is_announced_as_a_maybe_rather_than_a_question() {
    // Without hooks: an ordinary tool call whose result never came, quiet for
    // longer than the blocked-tool dwell. It is announced once it has also
    // waited the prompt dwell.
    let harness = engine();
    let now = SystemTime::now();
    let stuck = |at: SystemTime| {
        index(vec![Session {
            status: SessionStatus::Working,
            session_mtime: at - Duration::from_secs(150),
            last_entry: Some(tool_entry("Bash")),
            ..a_session("perm-sess", "/repo/x")
        }])
    };

    harness
        .inner()
        .notify_awaiting_decisions(&stuck(now), &HashSet::new(), now);
    assert!(harness.state().notifications.is_empty());

    let later = now + DWELL;
    harness
        .inner()
        .notify_awaiting_decisions(&stuck(later), &HashSet::new(), later);
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

/// The person at the keyboard answers most prompts at once. Those are not
/// news, and a row and a sound for each was most of the noise.
#[test]
fn a_prompt_answered_inside_the_dwell_is_never_announced() {
    let harness = engine();
    let now = SystemTime::now();
    let ids = hooked(&["hook-sess"]);

    harness
        .inner()
        .notify_awaiting_decisions(&index(vec![prompting("hook-sess", now)]), &ids, now);
    let answered = Session {
        status: SessionStatus::Working,
        ..prompting("hook-sess", now)
    };
    let soon = now + DWELL - SECOND;
    harness
        .inner()
        .notify_awaiting_decisions(&index(vec![answered]), &ids, soon);

    assert!(harness.state().notifications.is_empty());
    assert!(harness.state().awaits.is_empty());
}

#[test]
fn a_hook_reported_prompt_is_announced_after_the_dwell_and_deleted_when_it_ends() {
    let harness = engine();
    let now = SystemTime::now();
    let ids = hooked(&["hook-sess"]);
    let waiting = index(vec![prompting("hook-sess", now)]);

    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now);
    assert!(harness.state().notifications.is_empty());

    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now + DWELL);
    let row = {
        let state = harness.state();
        assert_eq!(state.notifications.len(), 1);
        assert!(state.notifications[0]
            .title
            .contains("may be waiting on a prompt"));
        state.notifications[0].id.clone()
    };

    // Approved: PostToolUse makes it working, and the row is deleted, in the
    // feed and in SQLite.
    let running = index(vec![Session {
        status: SessionStatus::Working,
        session_mtime: now - Duration::from_secs(600),
        last_entry: Some(tool_entry("Bash")),
        ..a_session("hook-sess", "/repo/x")
    }]);
    harness
        .inner()
        .notify_awaiting_decisions(&running, &ids, now + DWELL + SECOND);
    assert!(harness.state().notifications.is_empty());
    assert!(!harness.state().awaits.contains_key("hook-sess"));
    assert!(harness
        .engine
        .db()
        .recent_notifications(10)
        .iter()
        .all(|n| n.id != row));
}

/// A prompt still waiting is announced again, as the only row: the old one is
/// deleted first.
#[test]
fn a_prompt_still_waiting_is_raised_again_without_a_second_row() {
    let harness = engine();
    let now = SystemTime::now();
    let ids = hooked(&["hook-sess"]);
    let waiting = index(vec![prompting("hook-sess", now)]);

    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now);
    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now + DWELL);
    let first = harness.state().notifications[0].id.clone();

    // Not yet due: nothing changes.
    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now + DWELL + REPEAT - SECOND);
    assert_eq!(harness.state().notifications[0].id, first);

    harness
        .inner()
        .notify_awaiting_decisions(&waiting, &ids, now + DWELL + REPEAT);
    let rows = open_rows(&harness);
    assert_eq!(rows.len(), 1, "the reminder stacked a second row");
    assert_ne!(rows[0].id, first, "the reminder did not raise a new row");
    let stored = harness.engine.db().recent_notifications(10);
    assert_eq!(stored.len(), 1, "the old row stayed in SQLite");
}

/// A session that was killed, or went away, no longer waits on anyone. Its
/// question used to stay unresolved and keep "asks you" up for good, because
/// only a live session could end a wait.
#[test]
fn a_session_that_goes_away_resolves_its_question_and_deletes_its_prompt() {
    let harness = engine();
    let now = SystemTime::now();
    let ids = hooked(&["prompt-sess"]);
    let both = index(vec![asking("ask-sess"), prompting("prompt-sess", now)]);
    harness.inner().notify_awaiting_decisions(&both, &ids, now);
    harness
        .inner()
        .notify_awaiting_decisions(&both, &ids, now + DWELL);
    assert_eq!(open_rows(&harness).len(), 2);

    harness
        .inner()
        .notify_awaiting_decisions(&index(Vec::new()), &ids, now + DWELL + SECOND);

    assert!(open_rows(&harness).is_empty());
    let state = harness.state();
    assert!(state.awaits.is_empty());
    assert_eq!(
        state.notifications.len(),
        1,
        "the prompt row was not deleted"
    );
    assert_eq!(
        state.notifications[0].kind,
        crate::types::NotificationKind::Question
    );
}

/// A failed process read lists nothing. That is not every session going away:
/// ending their waits would clear the rows, and the next good scan would raise
/// them again, each with a sound.
#[test]
fn an_incomplete_scan_ends_no_wait() {
    let harness = engine();
    let now = SystemTime::now();
    harness.inner().notify_awaiting_decisions(
        &index(vec![asking("ask-sess")]),
        &HashSet::new(),
        now,
    );

    harness
        .inner()
        .notify_awaiting_decisions_in(&index(Vec::new()), &HashSet::new(), now, false);
    assert_eq!(
        open_rows(&harness).len(),
        1,
        "an outage cleared the question"
    );
    assert!(harness.state().awaits.contains_key("ask-sess"));

    // A session the incomplete scan did see, and that stopped waiting, still
    // ends its wait.
    harness.inner().notify_awaiting_decisions_in(
        &index(vec![a_session("ask-sess", "/repo/x")]),
        &HashSet::new(),
        now,
        false,
    );
    assert!(open_rows(&harness).is_empty());
}

/// After a restart the feed comes back from SQLite with no record of which
/// session each row was for. A row whose session still waits is adopted, so it
/// does not ring again. A row whose session no longer waits is closed.
#[test]
fn after_a_restart_restored_rows_are_adopted_or_closed() {
    let first = engine();
    let now = SystemTime::now();
    first.inner().notify_awaiting_decisions(
        &index(vec![asking("still"), asking("done")]),
        &HashSet::new(),
        now,
    );
    let restored = first.engine.db().recent_notifications(10);
    assert_eq!(restored.len(), 2);

    let second = engine();
    second.state().notifications = restored.into_iter().collect();
    let events = second.engine.subscribe();
    second
        .inner()
        .notify_awaiting_decisions(&index(vec![asking("still")]), &HashSet::new(), now);

    let rows = open_rows(&second);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id.as_deref(), Some("still"));
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, EngineEvent::Notification(_))),
        "a restored question rang again"
    );
    assert_eq!(
        second.state().awaits["still"].row.as_deref(),
        Some(rows[0].id.as_str())
    );
}

/// A QA reviewer's sessions prompt on every edit and command, so a prompt row
/// joins the feed without a sound. A question still rings.
#[test]
fn for_the_qa_role_a_prompt_is_silent_and_a_question_rings() {
    let harness = engine_with(Setup {
        config: Some(serde_json::json!({ "role": "qa" })),
        ..Setup::default()
    });
    let now = SystemTime::now();
    let ids = hooked(&["prompt-sess"]);
    let both = index(vec![asking("ask-sess"), prompting("prompt-sess", now)]);
    let events = harness.engine.subscribe();
    harness.inner().notify_awaiting_decisions(&both, &ids, now);
    harness
        .inner()
        .notify_awaiting_decisions(&both, &ids, now + DWELL);

    let (mut rang, mut silent) = (Vec::new(), Vec::new());
    for event in events.try_iter() {
        match event {
            EngineEvent::Notification(n) => rang.push(n.session_id.unwrap_or_default()),
            EngineEvent::NotificationUpdated(n) => silent.push(n.session_id.unwrap_or_default()),
            _ => {}
        }
    }
    assert_eq!(rang, ["ask-sess"]);
    assert_eq!(silent, ["prompt-sess"]);
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
    let notification = crate::daemon::watchers::awaiting_notification(&session, true, true);

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
    let notification = crate::daemon::watchers::awaiting_notification(&session, false, true);

    assert_eq!(notification.kind, crate::types::NotificationKind::Info);
    assert!(notification
        .message
        .contains("Nothing can answer one of those remotely"));
}
