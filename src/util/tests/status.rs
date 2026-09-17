//! The status machine and the activity label — the two things the sessions tree
//! reads off a transcript's trailing entry.

use crate::transcript::fixtures::{name, transcripts};
use crate::transcript::{parse_session_file, Entry};
use crate::types::{Color, EntryKind, LastEntry, MessageRole, ProgressData, SessionStatus};
use crate::util::{activity_label, detect_session_status};
use std::collections::BTreeSet;
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
fn a_pending_tool_call_is_working_however_long_it_runs() {
    // This used to flip to "awaiting" after thirty seconds. Duration says
    // nothing about whose turn it is: a build, a test suite, a browser step and
    // a sleep loop all run for minutes. Calling those "awaiting" reported that a
    // session wanted you when it was busy — one task sat four minutes into a
    // shell script reading "awaiting" while its own activity label said
    // "running command".
    //
    // A permission prompt is indistinguishable from a slow tool in the
    // transcript, and is handled where it belongs: the daemon raises a "may be
    // blocked" notification after a couple of minutes of silence.
    let entry = assistant_tool_use("Bash");
    for ago in [20, 45, 240, 3_600] {
        assert_eq!(
            detect_session_status(Some(&entry), seconds_ago(ago), now()),
            SessionStatus::Working,
            "{ago}s into a tool call"
        );
    }
}

#[test]
fn a_question_is_awaiting_immediately_however_fresh_the_write() {
    // Checked ahead of the freshness rule: the agent has handed control back,
    // so there is nothing recency can add. Without this the row read "working"
    // for the first ten seconds of every question asked.
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        let entry = assistant_tool_use(tool);
        assert_eq!(
            detect_session_status(Some(&entry), now(), now()),
            SessionStatus::Awaiting,
            "{tool} should be awaiting the moment it lands"
        );
    }
}

#[test]
fn a_tool_result_means_the_agent_is_thinking_not_waiting_on_you() {
    // A tool's output came back and the agent has not spoken yet: it is
    // thinking, or running the next tool. A long reasoning block routinely
    // passes thirty seconds, and nobody is waiting on the person.
    let mut entry = crate::types::LastEntry::of_kind(crate::types::EntryKind::User);
    entry.has_tool_result = true;
    for ago in [45, 300] {
        assert_eq!(
            detect_session_status(Some(&entry), seconds_ago(ago), now()),
            SessionStatus::Working,
            "{ago}s after a tool result"
        );
    }
}

#[test]
fn something_a_person_typed_still_goes_to_awaiting_input() {
    // The other kind of `user` entry. Unlike a tool result, this one really is
    // waiting on a reply.
    let entry = crate::types::LastEntry::of_kind(crate::types::EntryKind::User);
    assert_eq!(
        detect_session_status(Some(&entry), seconds_ago(60), now()),
        SessionStatus::AwaitingInput
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
    // The companion canary, over real transcripts, is the test below.
}

#[test]
fn known_gap_canary_no_real_transcript_ends_on_a_conversational_entry() {
    // The proof that the gap above is real rather than theoretical: not one of
    // the eight scrubbed sessions ends on a `user` or `assistant` line, so every
    // one of them reports idle the moment it stops being freshly written — no
    // matter what Claude was actually doing. Fixing the gap means teaching the
    // machine about the bookkeeping types; when this test starts failing,
    // Claude Code went back to ending transcripts on the conversation itself and
    // the branches above became reachable again.
    for path in transcripts() {
        let file = name(&path);
        let parsed = parse_session_file(&path).expect("fixture reads");
        let last = parsed.last_entry.expect("a transcript has a last entry");
        assert!(
            !matches!(last.kind, EntryKind::User | EntryKind::Assistant),
            "{file} now ends on a {} entry — the status machine can see the \
             conversation again, so revisit the known gap",
            last.kind.as_str()
        );
        assert_eq!(
            detect_session_status(Some(&last), seconds_ago(30), now()),
            SessionStatus::Idle,
            "{file}: a real transcript reports something other than idle"
        );
    }
}

#[test]
fn every_entry_type_in_the_fixtures_is_one_we_have_considered() {
    // Claude Code emits far more entry types than the parser consumes. This says
    // the ignoring is deliberate: a type that appears in regenerated fixtures and
    // is on neither list fails here, so somebody decides whether the parser or
    // the status machine should care before it is quietly dropped.
    //
    // Drives the status machine and the conversation pane:
    const HANDLED: &[&str] = &["user", "assistant", "system", "progress"];
    // Seen in real transcripts and deliberately ignored — bookkeeping that
    // carries no conversation. Every one of these reaching the status machine as
    // the trailing entry is the known gap above.
    const IGNORED: &[&str] = &[
        "summary",
        "attachment",
        "ai-title",
        "last-prompt",
        "mode",
        "permission-mode",
        "atis-latch",
        "pr-link",
        "queue-operation",
        "file-history-snapshot",
        "file-history-delta",
        "agent-name",
        "x-claude-md",
    ];

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for path in transcripts() {
        let body = std::fs::read_to_string(&path).expect("fixture reads");
        for line in body.lines() {
            if let Some(entry) = Entry::parse_line(line) {
                seen.insert(entry.last_entry().kind.as_str().to_string());
            }
        }
    }

    let unknown: Vec<&str> = seen
        .iter()
        .map(String::as_str)
        .filter(|t| !t.is_empty() && !HANDLED.contains(t) && !IGNORED.contains(t))
        .collect();
    assert!(
        unknown.is_empty(),
        "new transcript entry type(s): {} — decide whether the parser must handle \
         them, then add them to HANDLED or IGNORED",
        unknown.join(", ")
    );
    // And the reverse: the fixtures must still exercise the types we do handle,
    // or the suite has stopped testing what it claims to.
    for kind in ["user", "assistant", "system"] {
        assert!(
            seen.contains(kind),
            "no {kind} entries left in the fixtures"
        );
    }
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
    // working so the day they return the label is right. The canary below is
    // what proves they are absent.
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

#[test]
fn known_gap_canary_no_real_transcript_carries_a_progress_entry() {
    // Pairs with the label test above: the `progress` branches of both the label
    // and the status machine are unreachable on current Claude Code. If this
    // fails, progress entries are back — which also means `detect_session_status`
    // will start reporting `working` from them again.
    for path in transcripts() {
        let file = name(&path);
        let body = std::fs::read_to_string(&path).expect("fixture reads");
        for (n, line) in body.lines().enumerate() {
            let Some(entry) = Entry::parse_line(line) else {
                continue;
            };
            assert!(
                !matches!(entry.last_entry().kind, EntryKind::Progress),
                "{file}:{} is a progress entry — the unreachable branches are live again",
                n + 1
            );
        }
    }
}
