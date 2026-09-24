//! The status machine and the activity label — the two things the sessions tree
//! reads off a transcript.
//!
//! Most of these tests write a real line sequence to a file and fold it
//! through the real parser, because the bug they pin lived between the two:
//! the machine was right about each entry and wrong about which entry it was
//! handed. Current Claude Code ends almost every transcript on bookkeeping
//! (hook results, token reminders, titles, modes), and the sequences below keep
//! that noise in, as the real files have it.

use std::collections::BTreeSet;
use std::io::Write;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

use crate::hook_state::session_status;
use crate::transcript::fixtures::{name, transcripts};
use crate::transcript::{parse_session_file, Entry};
use crate::types::{Color, EntryKind, LastEntry, MessageRole, ProgressData, SessionStatus};
use crate::util::{activity_label, activity_time, detect_session_status, GENERATION_STALL};

// --- building transcripts -----------------------------------------------------

/// Second zero of every sequence. Offsets below are seconds after it.
fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-22T16:00:00.000Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn stamp(secs: f64) -> String {
    (t0() + chrono::Duration::milliseconds((secs * 1000.0) as i64))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The instant `secs` after second zero.
fn at(secs: u64) -> SystemTime {
    SystemTime::from(t0()) + Duration::from_secs(secs)
}

fn prompt(secs: f64, text: &str) -> Value {
    json!({ "type": "user", "userType": "external", "timestamp": stamp(secs),
            "message": { "role": "user", "content": text } })
}

fn meta_user(secs: f64, text: &str) -> Value {
    json!({ "type": "user", "userType": "external", "isMeta": true, "timestamp": stamp(secs),
            "message": { "role": "user", "content": [ { "type": "text", "text": text } ] } })
}

fn tool_use(secs: f64, tool: &str) -> Value {
    json!({ "type": "assistant", "timestamp": stamp(secs),
            "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": "toolu_1", "name": tool, "input": {} } ] } })
}

fn tool_result(secs: f64, content: &str, is_error: bool) -> Value {
    json!({ "type": "user", "userType": "external", "timestamp": stamp(secs),
            "toolUseResult": if is_error { json!("Error") } else { json!({ "stdout": content }) },
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": content,
                  "is_error": is_error } ] } })
}

fn thinking(secs: f64) -> Value {
    json!({ "type": "assistant", "timestamp": stamp(secs),
            "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "", "signature": "x" } ] } })
}

fn prose(secs: f64) -> Value {
    json!({ "type": "assistant", "timestamp": stamp(secs),
            "message": { "role": "assistant", "content": [ { "type": "text", "text": "Done." } ] } })
}

fn system(secs: f64, subtype: &str) -> Value {
    json!({ "type": "system", "subtype": subtype, "timestamp": stamp(secs) })
}

fn interrupt(secs: f64, for_tool_use: bool) -> Value {
    let text = if for_tool_use {
        "[Request interrupted by user for tool use]"
    } else {
        "[Request interrupted by user]"
    };
    json!({ "type": "user", "userType": "external", "timestamp": stamp(secs),
            "message": { "role": "user", "content": [ { "type": "text", "text": text } ] } })
}

/// The hook result an installed PreToolUse / PostToolUse / Stop hook leaves
/// after the line it ran for.
fn hook_success(secs: f64) -> Value {
    json!({ "type": "attachment", "timestamp": stamp(secs),
            "attachment": { "type": "hook_success", "hookEvent": "PostToolUse" } })
}

fn tokens_reminder(secs: f64) -> Value {
    json!({ "type": "attachment", "timestamp": stamp(secs),
            "attachment": { "type": "total_tokens_reminder" } })
}

/// The untimed bookkeeping block Claude Code appends in bursts.
fn bookkeeping() -> Vec<Value> {
    vec![
        json!({ "type": "last-prompt", "lastPrompt": "x" }),
        json!({ "type": "mode", "mode": "default" }),
        json!({ "type": "permission-mode", "permissionMode": "default" }),
        json!({ "type": "atis-latch" }),
        json!({ "type": "ai-title", "aiTitle": "x" }),
        json!({ "type": "cost-state" }),
        json!({ "type": "file-history-snapshot", "messageId": "m" }),
    ]
}

/// A permission denial with no instructions: Claude Code writes the rejected
/// result, then the interrupt marker.
fn denial(secs: f64) -> Vec<Value> {
    vec![
        tool_result(
            secs,
            "The user doesn't want to proceed with this tool use. The tool use was rejected \
             (eg. if it was a file edit, the new_string was NOT written to the file). STOP what \
             you are doing and wait for the user to tell you how to proceed.",
            true,
        ),
        interrupt(secs + 0.001, true),
    ]
}

/// The end of a turn with a Stop hook installed.
fn turn_end(secs: f64) -> Vec<Value> {
    vec![
        prose(secs),
        hook_success(secs + 0.2),
        system(secs + 0.21, "stop_hook_summary"),
        system(secs + 0.22, "turn_duration"),
    ]
}

/// Fold `lines` through the real parser, as a cold start does.
fn fold(lines: &[Value]) -> Option<LastEntry> {
    let mut file = tempfile::NamedTempFile::new().expect("temp transcript");
    for line in lines {
        writeln!(file, "{line}").expect("write line");
    }
    parse_session_file(file.path()).expect("parse").last_entry
}

/// The transcript-only status of `lines` at `now`. The mtime is deliberately
/// "now": bookkeeping writes keep it fresh, and it must not matter.
fn status(lines: &[Value], now: SystemTime) -> SessionStatus {
    let last = fold(lines);
    session_status(last.as_ref(), now, None, now)
}

fn seq(parts: Vec<Vec<Value>>) -> Vec<Value> {
    parts.into_iter().flatten().collect()
}

// --- realistic sequences ------------------------------------------------------

#[test]
fn a_tool_run_with_trailing_hook_results_is_working() {
    // The shape that used to read "idle" ten seconds into every tool call: the
    // hook result and the token reminder land after the tool call and after
    // its result, and they were the "last entry".
    let lines = seq(vec![
        vec![prompt(0.0, "run the tests")],
        vec![tool_use(2.0, "Bash"), hook_success(2.2)],
        bookkeeping(),
    ]);
    for secs in [5, 30, 90] {
        assert_eq!(
            status(&lines, at(secs)),
            SessionStatus::Working,
            "{secs}s into the tool call"
        );
    }
}

#[test]
fn a_long_build_stays_working_with_no_time_limit() {
    // A pending tool call is the agent working. A permission prompt looks the
    // same in the transcript, and the hooks tell the two apart.
    let lines = seq(vec![
        vec![
            prompt(0.0, "build it"),
            tool_use(1.0, "Bash"),
            hook_success(1.2),
        ],
        vec![tokens_reminder(1.3)],
    ]);
    for secs in [300, 3_600] {
        assert_eq!(status(&lines, at(secs)), SessionStatus::Working, "{secs}s");
    }
}

#[test]
fn a_tool_result_followed_by_hook_noise_is_working() {
    let lines = seq(vec![
        vec![prompt(0.0, "go"), tool_use(1.0, "Read")],
        vec![tool_result(2.0, "file body", false), hook_success(2.0)],
        vec![tokens_reminder(2.1)],
        bookkeeping(),
    ]);
    assert_eq!(status(&lines, at(45)), SessionStatus::Working);
}

#[test]
fn a_denied_permission_prompt_is_idle_at_once() {
    // This read "awaiting" forever: the interrupt marker is a user line with
    // no tool result, which the old machine took for fresh input.
    let lines = seq(vec![
        vec![
            prompt(0.0, "edit it"),
            tool_use(1.0, "Edit"),
            hook_success(1.1),
        ],
        denial(20.0),
        bookkeeping(),
    ]);
    for secs in [21, 60, 3_600] {
        assert_eq!(status(&lines, at(secs)), SessionStatus::Idle, "{secs}s");
    }
}

#[test]
fn a_denial_with_instructions_keeps_the_agent_working() {
    // "To tell you how to proceed, the user said: …" — the agent carries on,
    // and no interrupt marker follows.
    let lines = vec![
        prompt(0.0, "edit it"),
        tool_use(1.0, "Edit"),
        tool_result(
            20.0,
            "The user doesn't want to proceed with this tool use. The tool use was rejected. \
             To tell you how to proceed, the user said:\nuse the other file",
            true,
        ),
    ];
    assert_eq!(status(&lines, at(40)), SessionStatus::Working);
}

#[test]
fn escape_mid_turn_is_idle() {
    let lines = seq(vec![
        vec![prompt(0.0, "think hard"), thinking(5.0)],
        vec![interrupt(8.0, false)],
        bookkeeping(),
    ]);
    assert_eq!(status(&lines, at(9)), SessionStatus::Idle);
}

#[test]
fn a_pending_question_is_awaiting_through_the_hook_noise() {
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        let lines = seq(vec![
            vec![
                prompt(0.0, "plan it"),
                tool_use(3.0, tool),
                hook_success(3.2),
            ],
            bookkeeping(),
        ]);
        for secs in [3, 10, 3_600] {
            assert_eq!(
                status(&lines, at(secs)),
                SessionStatus::Awaiting,
                "{tool} at {secs}s"
            );
        }
    }
}

#[test]
fn an_answered_question_is_working_then_idle_when_the_turn_ends() {
    let asked = vec![
        prompt(0.0, "plan it"),
        tool_use(3.0, "AskUserQuestion"),
        hook_success(3.2),
    ];
    let answered = seq(vec![
        asked,
        vec![
            tool_result(40.0, "User answered: option 2", false),
            hook_success(40.1),
        ],
    ]);
    assert_eq!(status(&answered, at(45)), SessionStatus::Working);

    let finished = seq(vec![answered, turn_end(60.0), bookkeeping()]);
    assert_eq!(status(&finished, at(61)), SessionStatus::Idle);
}

#[test]
fn a_declined_question_is_idle() {
    let lines = seq(vec![
        vec![prompt(0.0, "ask me"), tool_use(3.0, "AskUserQuestion")],
        denial(30.0),
    ]);
    assert_eq!(status(&lines, at(31)), SessionStatus::Idle);
}

#[test]
fn a_finished_turn_is_idle_even_while_the_file_is_fresh() {
    // The turn-end line is unambiguous, so the old ten-second "fresh write"
    // rule no longer turns it into "working".
    let lines = seq(vec![
        vec![prompt(0.0, "hi")],
        turn_end(4.0),
        bookkeeping(),
        vec![system(200.0, "away_summary")],
    ]);
    assert_eq!(status(&lines, at(5)), SessionStatus::Idle);
    assert_eq!(status(&lines, at(3_600)), SessionStatus::Idle);
}

#[test]
fn an_api_error_ends_the_turn() {
    let lines = vec![
        prompt(0.0, "go"),
        json!({ "type": "assistant", "isApiErrorMessage": true, "timestamp": stamp(3.0),
                "message": { "role": "assistant", "content": [
                    { "type": "text", "text": "API Error: Connection lost mid-response." } ] } }),
        system(3.1, "turn_duration"),
    ];
    assert_eq!(status(&lines, at(4)), SessionStatus::Idle);
}

#[test]
fn the_agent_thinking_mid_turn_is_working_until_the_stall_cap() {
    // A long reasoning block passes a minute routinely. Nobody waits on the
    // person, so this is never "awaiting".
    let lines = seq(vec![
        vec![prompt(0.0, "go"), tool_use(1.0, "Read")],
        vec![tool_result(2.0, "body", false), hook_success(2.0)],
        vec![thinking(20.0), tokens_reminder(20.1)],
    ]);
    assert_eq!(status(&lines, at(110)), SessionStatus::Working);
    let stalled = 20 + GENERATION_STALL.as_secs();
    assert_eq!(status(&lines, at(stalled)), SessionStatus::Idle);
}

#[test]
fn prose_mid_turn_is_working() {
    // Claude Code writes each content block as its own line, so prose is
    // followed by the tool call it introduced. A finished reply is followed by
    // `turn_duration` within a second instead.
    let lines = vec![prompt(0.0, "go"), prose(5.0), hook_success(5.1)];
    assert_eq!(status(&lines, at(70)), SessionStatus::Working);
}

#[test]
fn a_prompt_with_no_reply_is_working_and_never_awaiting() {
    // The old machine called this "awaiting input" after thirty seconds. The
    // person has typed. The agent is thinking, or the turn died.
    let lines = seq(vec![vec![prompt(0.0, "go")], bookkeeping()]);
    assert_eq!(status(&lines, at(45)), SessionStatus::Working);
    assert_eq!(status(&lines, at(300)), SessionStatus::Working);
    assert_eq!(
        status(&lines, at(GENERATION_STALL.as_secs())),
        SessionStatus::Idle
    );
}

#[test]
fn a_skill_body_injected_after_a_tool_result_is_working() {
    let lines = vec![
        prompt(0.0, "/qa 12"),
        tool_use(1.0, "Skill"),
        tool_result(1.2, "Launching skill: qa", false),
        meta_user(1.1, "Base directory for this skill: /x"),
    ];
    assert_eq!(status(&lines, at(30)), SessionStatus::Working);
}

#[test]
fn local_command_output_is_idle() {
    // `/usage` writes its output as a system line; older commands write it as
    // a user line. The agent answers neither.
    let as_system = vec![
        meta_user(
            0.0,
            "<local-command-caveat>Caveat: do not respond</local-command-caveat>",
        ),
        prompt(0.1, "<command-name>/usage</command-name>"),
        json!({ "type": "system", "subtype": "local_command", "timestamp": stamp(0.2),
                "content": "<local-command-stdout>64% used</local-command-stdout>" }),
    ];
    assert_eq!(status(&as_system, at(1)), SessionStatus::Idle);

    let as_user = vec![
        meta_user(0.0, "<local-command-caveat>Caveat</local-command-caveat>"),
        prompt(0.1, "<command-name>/model</command-name>"),
        prompt(
            0.2,
            "<local-command-stdout>Set model to Opus</local-command-stdout>",
        ),
    ];
    assert_eq!(status(&as_user, at(1)), SessionStatus::Idle);
}

#[test]
fn a_shell_command_the_person_ran_is_idle() {
    let lines = vec![
        prompt(0.0, "<bash-input>git pull</bash-input>"),
        prompt(
            0.5,
            "<bash-stdout>Already up to date.</bash-stdout><bash-stderr></bash-stderr>",
        ),
    ];
    assert_eq!(status(&lines, at(1)), SessionStatus::Idle);
}

#[test]
fn out_of_order_timestamps_age_from_the_newest() {
    // Real transcripts write a tool result, then a meta line stamped earlier.
    // Ageing from the last stamp read would make the session older than it is.
    let lines = vec![
        prompt(0.0, "go"),
        tool_use(1.0, "Skill"),
        tool_result(100.0, "Launching skill", false),
        meta_user(40.0, "Base directory for this skill: /x"),
    ];
    let last = fold(&lines).expect("a last entry");
    assert_eq!(activity_time(Some(&last), at(0)), at(100));
    // One second inside the cap from the newest stamp, although past it from
    // the meta line's own stamp.
    let now = at(100 + GENERATION_STALL.as_secs() - 1);
    assert_eq!(status(&lines, now), SessionStatus::Working);
}

#[test]
fn a_new_bookkeeping_type_changes_nothing() {
    // The allowlist is the point: a kind Claude Code adds next month is
    // ignored, where the old fall-through made it "idle".
    let lines = vec![
        prompt(0.0, "go"),
        tool_use(1.0, "Bash"),
        json!({ "type": "brand-new-kind", "timestamp": stamp(2.0) }),
        json!({ "type": "system", "subtype": "brand_new_subtype", "timestamp": stamp(2.1) }),
    ];
    assert_eq!(status(&lines, at(60)), SessionStatus::Working);
}

#[test]
fn a_compaction_boundary_is_idle_and_the_summary_after_it_is_not() {
    let boundary = vec![prompt(0.0, "/compact"), system(30.0, "compact_boundary")];
    assert_eq!(status(&boundary, at(31)), SessionStatus::Idle);
    // An automatic compaction carries on into the turn it interrupted.
    let continued = seq(vec![
        boundary,
        vec![meta_user(
            30.1,
            "This session is being continued from a previous conversation.",
        )],
    ]);
    assert_eq!(status(&continued, at(35)), SessionStatus::Working);
}

#[test]
fn a_transcript_of_only_bookkeeping_is_idle() {
    assert_eq!(status(&bookkeeping(), at(1)), SessionStatus::Idle);
}

#[test]
fn the_mtime_is_only_a_fallback_clock() {
    // No stamps at all (as in some test harnesses): the mtime ages the entry.
    let untimed = LastEntry {
        kind: EntryKind::User,
        ..Default::default()
    };
    assert_eq!(activity_time(Some(&untimed), at(7)), at(7));
    // With a stamp, the mtime is ignored, however fresh.
    let timed = LastEntry {
        activity_at: Some(stamp(3.0)),
        ..untimed
    };
    assert_eq!(activity_time(Some(&timed), at(900)), at(3));
}

// --- the rules on hand-built entries ------------------------------------------

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

#[test]
fn a_question_is_awaiting_immediately() {
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        assert_eq!(
            detect_session_status(Some(&assistant_tool_use(tool)), at(0), at(0)),
            SessionStatus::Awaiting,
            "{tool}"
        );
    }
}

#[test]
fn every_system_entry_that_reaches_the_machine_is_idle() {
    for subtype in ["turn_duration", "compact_boundary", "local_command"] {
        let entry = LastEntry {
            kind: EntryKind::System,
            subtype: Some(subtype.to_string()),
            ..Default::default()
        };
        assert_eq!(
            detect_session_status(Some(&entry), at(0), at(0)),
            SessionStatus::Idle,
            "{subtype}"
        );
    }
}

#[test]
fn an_entry_that_ends_the_turn_is_idle_whatever_its_kind() {
    let entry = LastEntry {
        kind: EntryKind::User,
        has_tool_result: true,
        ends_turn: true,
        ..Default::default()
    };
    assert_eq!(
        detect_session_status(Some(&entry), at(0), at(1)),
        SessionStatus::Idle
    );
}

#[test]
fn progress_entries_mean_actively_working() {
    let entry = progress("bash_progress", None);
    assert_eq!(
        detect_session_status(Some(&entry), at(0), at(600)),
        SessionStatus::Working
    );
}

#[test]
fn no_entry_at_all_is_idle() {
    assert_eq!(
        detect_session_status(None, at(0), at(0)),
        SessionStatus::Idle
    );
}

#[test]
fn a_future_stamp_reads_as_age_zero_not_as_a_panic() {
    assert_eq!(
        detect_session_status(Some(&assistant_prose()), at(3_600), at(0)),
        SessionStatus::Working
    );
}

#[test]
fn nothing_produces_awaiting_input_any_more() {
    let entries = [
        LastEntry::of_kind(EntryKind::User),
        assistant_prose(),
        assistant_tool_use("Bash"),
        LastEntry::of_kind("some-kind"),
    ];
    for entry in entries {
        for secs in [0, 20, 45, 120, 3_600] {
            assert_ne!(
                detect_session_status(Some(&entry), at(0), at(secs)),
                SessionStatus::AwaitingInput,
                "{:?} at {secs}s",
                entry.kind
            );
        }
    }
}

// --- the real transcripts -----------------------------------------------------

#[test]
fn every_real_transcript_ends_on_a_conversational_entry() {
    // The canary this suite used to carry said the opposite: that no scrubbed
    // session ended on a user or assistant line, so every one read "idle".
    // With bookkeeping ignored, the trailing entry is always the conversation,
    // and the status half a minute after it matches what the session was doing.
    let expected = [
        ("session-01.jsonl", SessionStatus::Working), // a tool result
        ("session-02.jsonl", SessionStatus::Working), // a tool result
        ("session-03.jsonl", SessionStatus::Working), // a tool result
        ("session-04.jsonl", SessionStatus::Idle),    // turn_duration
        ("session-05.jsonl", SessionStatus::Working), // a pending Bash call
        ("session-06.jsonl", SessionStatus::Working), // scrubbed user lines
        ("session-07.jsonl", SessionStatus::Idle),    // turn_duration
        ("session-08.jsonl", SessionStatus::Idle),    // turn_duration
    ];
    let paths = transcripts();
    assert_eq!(paths.len(), expected.len(), "the fixture set changed");
    for (path, (file, want)) in paths.iter().zip(expected) {
        assert_eq!(name(path), file);
        let parsed = parse_session_file(path).expect("fixture reads");
        let last = parsed.last_entry.expect("a transcript has a last entry");
        assert!(
            matches!(
                last.kind,
                EntryKind::User | EntryKind::Assistant | EntryKind::System
            ),
            "{file} ends on a {} entry",
            last.kind.as_str()
        );
        let active = last.activity_instant().expect("fixtures carry stamps");
        let now = active + Duration::from_secs(30);
        assert_eq!(session_status(Some(&last), now, None, now), want, "{file}");
    }
}

#[test]
fn every_entry_type_in_the_fixtures_is_one_we_have_considered() {
    // Claude Code emits far more entry types than the status machine reads.
    // This says the ignoring is deliberate: a type that appears in regenerated
    // fixtures and is on neither list fails here, so somebody decides whether
    // it belongs on the allowlist in `Entry::drives_status`.
    //
    // Drive the status machine and the conversation pane:
    const HANDLED: &[&str] = &["user", "assistant", "system", "progress"];
    // Seen in real transcripts and deliberately ignored — bookkeeping that
    // carries no conversation.
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
        "cost-state",
        "bridge-session",
        "artifact-autoreact-ledger",
        "artifact-comment-monitor",
        "frame-link",
        "continued-in",
    ];

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for path in transcripts() {
        let body = std::fs::read_to_string(&path).expect("fixture reads");
        for line in body.lines() {
            if let Some(entry) = Entry::parse_line(line) {
                let kind = entry.last_entry().kind.as_str().to_string();
                // The allowlist and this list must agree.
                if IGNORED.contains(&kind.as_str()) {
                    assert!(!entry.drives_status(), "{kind} drives the status");
                }
                seen.insert(kind);
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
        "new transcript entry type(s): {} — decide whether the status machine must \
         read them, then add them to HANDLED or IGNORED",
        unknown.join(", ")
    );
    for kind in ["user", "assistant", "system"] {
        assert!(
            seen.contains(kind),
            "no {kind} entries left in the fixtures"
        );
    }
}

#[test]
fn only_the_turn_changing_system_subtypes_drive_the_status() {
    let drives = |subtype: &str| {
        Entry::parse_line(&json!({ "type": "system", "subtype": subtype }).to_string())
            .expect("parses")
            .drives_status()
    };
    for subtype in ["turn_duration", "compact_boundary", "local_command"] {
        assert!(drives(subtype), "{subtype}");
    }
    for subtype in [
        "stop_hook_summary",
        "away_summary",
        "informational",
        "bridge_status",
    ] {
        assert!(!drives(subtype), "{subtype}");
    }
}

#[test]
fn status_labels_and_colours_cover_every_variant() {
    use SessionStatus::*;
    assert_eq!(Working.label(), "working");
    assert_eq!(Idle.label(), "idle");
    // An older daemon can still send `AwaitingInput`, and it reads the same.
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
fn activity_label_follows_the_conversation_not_the_bookkeeping() {
    // The hook result after a tool call used to blank the label.
    let lines = vec![prompt(0.0, "go"), tool_use(1.0, "Bash"), hook_success(1.2)];
    assert_eq!(activity_label(fold(&lines).as_ref()), "running command");
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
