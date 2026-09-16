//! The status machine and the activity label — the two things the sessions tree
//! reads off a transcript's trailing entry.

use crate::types::{Color, EntryKind, LastEntry, MessageRole, ProgressData, SessionStatus};
use crate::util::{activity_label, detect_session_status};
use std::time::{Duration, SystemTime};

/// A fixed "now" so every age in these tests is exact.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn seconds_ago(secs: u64) -> SystemTime {
    now() - Duration::from_secs(secs)
}

fn assistant_tool_use(name: &str) -> LastEntry {
    LastEntry {
        kind: EntryKind::Assistant,
        role: Some(MessageRole::Assistant),
        has_message: true,
        tool_uses: vec![name.to_string()],
        ..Default::default()
    }
}

fn assistant_prose() -> LastEntry {
    LastEntry {
        kind: EntryKind::Assistant,
        role: Some(MessageRole::Assistant),
        has_message: true,
        ..Default::default()
    }
}

fn progress(kind: &str, hook: Option<&str>) -> LastEntry {
    LastEntry {
        kind: EntryKind::Progress,
        progress: Some(ProgressData {
            kind: Some(kind.to_string()),
            hook_name: hook.map(str::to_string),
        }),
        ..Default::default()
    }
}

// --- truncate ---------------------------------------------------------------

#[test]
fn a_file_written_in_the_last_10s_is_working_whatever_the_entry_says() {
    assert_eq!(
        detect_session_status(Some(&assistant_prose()), now(), now()),
        SessionStatus::Working
    );
    let turn_done = LastEntry {
        kind: EntryKind::System,
        subtype: Some("turn_duration".to_string()),
        ..Default::default()
    };
    assert_eq!(
        detect_session_status(Some(&turn_done), seconds_ago(9), now()),
        SessionStatus::Working
    );
}

#[test]
fn turn_duration_marks_the_turn_finished() {
    let turn_done = LastEntry {
        kind: EntryKind::System,
        subtype: Some("turn_duration".to_string()),
        ..Default::default()
    };
    assert_eq!(
        detect_session_status(Some(&turn_done), seconds_ago(60), now()),
        SessionStatus::Idle
    );
    // A system entry without that subtype is not a finished turn; it falls
    // through to idle by the same route as any unknown type.
    let other_system = LastEntry::of_kind("system");
    assert_eq!(
        detect_session_status(Some(&other_system), seconds_ago(60), now()),
        SessionStatus::Idle
    );
}

#[test]
fn a_pending_tool_call_is_working_briefly_then_awaiting() {
    let entry = assistant_tool_use("Bash");
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(20), now()),
        SessionStatus::Working
    );
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(45), now()),
        SessionStatus::Awaiting
    );
}

#[test]
fn prose_means_claude_is_waiting_on_you_then_goes_idle() {
    let entry = assistant_prose();
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(30), now()),
        SessionStatus::AwaitingInput
    );
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(120), now()),
        SessionStatus::Idle
    );
}

#[test]
fn a_user_entry_is_mid_turn_briefly_then_awaiting_input() {
    let entry = LastEntry::of_kind("user");
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(20), now()),
        SessionStatus::Working
    );
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(60), now()),
        SessionStatus::AwaitingInput
    );
}

#[test]
fn progress_entries_mean_actively_working() {
    let entry = progress("bash_progress", None);
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(600), now()),
        SessionStatus::Working
    );
}

#[test]
fn no_entry_at_all_is_idle() {
    assert_eq!(
        detect_session_status(None, now(), now()),
        SessionStatus::Idle
    );
}

#[test]
fn a_future_mtime_reads_as_working_not_as_a_panic() {
    let entry = assistant_prose();
    let future = now() + Duration::from_secs(3600);
    assert_eq!(
        detect_session_status(Some(&entry), future, now()),
        SessionStatus::Working
    );
}

#[test]
fn known_gap_modern_trailing_entry_types_fall_through_to_idle() {
    // Documented, not asserted-as-correct: current Claude Code ends transcripts
    // on these bookkeeping entries, so the conversational branches above are
    // mostly unreachable on live sessions. If this starts failing, status
    // detection was changed — update the machine and this test together.
    for kind in [
        "last-prompt",
        "file-history-snapshot",
        "attachment",
        "ai-title",
        "mode",
        "pr-link",
    ] {
        assert_eq!(
            detect_session_status(Some(&LastEntry::of_kind(kind)), seconds_ago(30), now()),
            SessionStatus::Idle,
            "{kind} no longer falls through to idle"
        );
    }
    // The companion canary — "no live transcript ends with a user/assistant
    // entry" — needs the scrubbed fixtures, and lands with the parser in phase 2.
}

#[test]
fn status_labels_and_colours_cover_every_variant() {
    use SessionStatus::*;
    assert_eq!(Working.label(), "working");
    assert_eq!(Idle.label(), "idle");
    // Both awaiting flavours read the same to the eye; only alerting tells them
    // apart.
    assert_eq!(Awaiting.label(), "awaiting");
    assert_eq!(AwaitingInput.label(), "awaiting");
    assert_eq!(Compacting.label(), "compacting");
    assert_eq!(Starting.label(), "starting…");
    assert_eq!(Working.color(), Color::Green);
    assert_eq!(Idle.color(), Color::Gray);
    assert_eq!(Awaiting.color(), Color::Yellow);
    assert_eq!(AwaitingInput.color(), Color::Cyan);
    assert_eq!(Compacting.color(), Color::Magenta);
    assert_eq!(Starting.color(), Color::Cyan);
}

// --- getActivityLabel -------------------------------------------------------

#[test]
fn activity_label_names_the_tool_in_flight() {
    let label = |name: &str| activity_label(Some(&assistant_tool_use(name)));
    assert_eq!(label("Read"), "reading");
    assert_eq!(label("Bash"), "running command");
    assert_eq!(label("Task"), "running agent");
    // Unlisted tools — every MCP one included — still say something useful.
    assert_eq!(label("SomeNewTool"), "using SomeNewTool");
}

#[test]
fn activity_label_uses_the_last_tool_call_in_a_multi_tool_turn() {
    let entry = LastEntry {
        kind: EntryKind::Assistant,
        has_message: true,
        tool_uses: vec!["Read".to_string(), "Bash".to_string()],
        ..Default::default()
    };
    assert_eq!(activity_label(Some(&entry)), "running command");
}

#[test]
fn activity_label_describes_prose_and_user_turns() {
    assert_eq!(activity_label(Some(&assistant_prose())), "responding");
    assert_eq!(
        activity_label(Some(&LastEntry::of_kind("user"))),
        "thinking"
    );
}

#[test]
fn activity_label_is_empty_for_unknown_and_missing_entries() {
    assert_eq!(activity_label(None), "");
    assert_eq!(activity_label(Some(&LastEntry::of_kind("last-prompt"))), "");
    assert_eq!(
        activity_label(Some(&LastEntry::of_kind("file-history-snapshot"))),
        ""
    );
    // An assistant entry with no message body at all says nothing either.
    assert_eq!(activity_label(Some(&LastEntry::of_kind("assistant"))), "");
}

#[test]
fn known_gap_progress_labels_still_work_but_are_unreachable() {
    // Current Claude Code emits no `progress` entries; the branch is kept
    // working so the day they return the label is right. The fixture-wide canary
    // that proves they are absent lands with the parser in phase 2.
    assert_eq!(
        activity_label(Some(&progress("bash_progress", None))),
        "running command"
    );
    assert_eq!(
        activity_label(Some(&progress("agent_progress", None))),
        "running agent"
    );
    assert_eq!(
        activity_label(Some(&progress("hook_progress", Some("PreToolUse:Read")))),
        "reading"
    );
    assert_eq!(
        activity_label(Some(&progress("hook_progress", Some("PreToolUse:Weird")))),
        "using Weird"
    );
    assert_eq!(
        activity_label(Some(&progress("hook_progress", None))),
        "processing"
    );
    assert_eq!(
        activity_label(Some(&progress("something_new", None))),
        "processing"
    );
}
