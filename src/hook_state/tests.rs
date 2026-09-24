//! The hook command's parsing and file handling, and how its state combines
//! with the transcript.

use std::fs;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;

use super::*;
use crate::paths::Paths;
use crate::types::{EntryKind, LastEntry, SessionStatus};

const SESSION: &str = "0b3c7e1a-5f2d-4c1e-9a77-2d1f00c0ffee";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-22T16:00:00.000Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// A payload shaped like the one Claude Code writes to a hook's stdin.
fn payload(event: &str, extra: serde_json::Value) -> Vec<u8> {
    let mut value = json!({
        "hook_event_name": event,
        "session_id": SESSION,
        "transcript_path": format!("/Users/me/.claude/projects/-repo/{SESSION}.jsonl"),
        "cwd": "/repo",
        "permission_mode": "default",
    });
    if let (Some(map), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        for (key, field) in more {
            map.insert(key.clone(), field.clone());
        }
    }
    value.to_string().into_bytes()
}

fn status_of(event: &str, extra: serde_json::Value) -> Option<SessionStatus> {
    match parse_hook_input(&payload(event, extra), now()) {
        HookAction::Record(state) => state.status(),
        _ => None,
    }
}

// --- parsing each event -------------------------------------------------------

#[test]
fn each_event_maps_to_its_status() {
    use SessionStatus::*;
    let cases = [
        ("UserPromptSubmit", json!({ "prompt": "go" }), Working),
        (
            "PreToolUse",
            json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } }),
            Working,
        ),
        (
            "PostToolUse",
            json!({ "tool_name": "Bash", "tool_response": { "stdout": "" } }),
            Working,
        ),
        (
            "SubagentStop",
            json!({ "stop_hook_active": false }),
            Working,
        ),
        (
            "PermissionRequest",
            json!({ "tool_name": "Bash", "tool_input": { "command": "rm x" } }),
            Awaiting,
        ),
        (
            "Notification",
            json!({ "notification_type": "permission_prompt",
                    "message": "Claude needs your permission to use Bash" }),
            Awaiting,
        ),
        (
            "Notification",
            json!({ "notification_type": "elicitation_dialog", "message": "?" }),
            Awaiting,
        ),
        (
            "Notification",
            json!({ "notification_type": "idle_prompt",
                    "message": "Claude is waiting for your input" }),
            Idle,
        ),
        ("Stop", json!({ "stop_hook_active": false }), Idle),
        ("StopFailure", json!({ "error": "rate_limit" }), Idle),
        ("SessionStart", json!({ "source": "startup" }), Idle),
    ];
    for (event, extra, want) in cases {
        assert_eq!(
            status_of(event, extra.clone()),
            Some(want),
            "{event} {extra}"
        );
    }
}

#[test]
fn a_question_tool_is_awaiting_from_its_pre_tool_use() {
    // Mapping it to working would let the hook, newer than the transcript's
    // tool call, overrule the transcript's correct "awaiting".
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        assert_eq!(
            status_of("PreToolUse", json!({ "tool_name": tool })),
            Some(SessionStatus::Awaiting),
            "{tool}"
        );
        // Answered: PostToolUse is working again.
        assert_eq!(
            status_of("PostToolUse", json!({ "tool_name": tool })),
            Some(SessionStatus::Working),
            "{tool}"
        );
    }
}

#[test]
fn a_recorded_state_keeps_the_raw_facts() {
    let HookAction::Record(state) = parse_hook_input(
        &payload(
            "Notification",
            json!({ "notification_type": "permission_prompt" }),
        ),
        now(),
    ) else {
        panic!("not recorded");
    };
    assert_eq!(
        state,
        HookState {
            session_id: SESSION.to_string(),
            event: "Notification".to_string(),
            tool_name: None,
            notification_type: Some("permission_prompt".to_string()),
            timestamp: "2026-09-22T16:00:00.000Z".to_string(),
        }
    );
}

#[test]
fn session_end_removes_and_events_with_no_say_are_ignored() {
    assert_eq!(
        parse_hook_input(&payload("SessionEnd", json!({ "reason": "exit" })), now()),
        HookAction::Remove {
            session_id: SESSION.to_string()
        }
    );
    // Recording these would replace a state that does have a say.
    for (event, extra) in [
        (
            "Notification",
            json!({ "notification_type": "auth_success" }),
        ),
        ("Notification", json!({})),
        ("PreCompact", json!({ "trigger": "auto" })),
        ("SomethingNew", json!({})),
    ] {
        assert_eq!(
            parse_hook_input(&payload(event, extra), now()),
            HookAction::Ignore,
            "{event}"
        );
    }
}

#[test]
fn unreadable_or_unsafe_input_is_ignored_never_an_error() {
    let ignored = |bytes: &[u8]| parse_hook_input(bytes, now()) == HookAction::Ignore;
    assert!(ignored(b""));
    assert!(ignored(b"not json"));
    assert!(ignored(b"[1,2,3]"));
    assert!(ignored(
        json!({ "session_id": SESSION }).to_string().as_bytes()
    ));
    assert!(ignored(
        json!({ "hook_event_name": "Stop" }).to_string().as_bytes()
    ));
    // A field of the wrong type costs that field, not the event.
    let HookAction::Record(state) =
        parse_hook_input(&payload("PreToolUse", json!({ "tool_name": 42 })), now())
    else {
        panic!("a bad tool_name dropped the whole event");
    };
    assert_eq!(state.tool_name, None);
    // The id becomes a file name, so a path in it is refused.
    for id in ["../../etc/passwd", "a/b", "", "x.json"] {
        let bytes = json!({ "hook_event_name": "Stop", "session_id": id }).to_string();
        assert!(ignored(bytes.as_bytes()), "{id:?}");
    }
}

// --- the state file -----------------------------------------------------------

fn temp_paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = Paths::for_test(dir.path());
    (dir, paths)
}

fn record(paths: &Paths, event: &str, extra: serde_json::Value) {
    apply(paths, &parse_hook_input(&payload(event, extra), now())).expect("applied");
}

#[test]
fn a_state_is_written_whole_and_replaced_atomically() {
    let (_dir, paths) = temp_paths();
    record(&paths, "PreToolUse", json!({ "tool_name": "Bash" }));
    let path = state_path(&paths, SESSION).expect("safe id");
    assert_eq!(
        path,
        paths
            .runtime_dir
            .join("hooks")
            .join(format!("{SESSION}.json"))
    );
    let first = read_state(&path).expect("state readable");
    assert_eq!(first.event, "PreToolUse");

    record(&paths, "PermissionRequest", json!({ "tool_name": "Bash" }));
    let second = read_state(&path).expect("state readable");
    assert_eq!(second.event, "PermissionRequest");
    assert_eq!(second.status(), Some(SessionStatus::Awaiting));

    // Nothing but the state file is left behind: the temporary file was
    // renamed into place, not copied.
    let names: Vec<String> = fs::read_dir(hooks_dir(&paths))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![format!("{SESSION}.json")]);
}

#[test]
fn session_end_deletes_the_state_file_and_tolerates_a_missing_one() {
    let (_dir, paths) = temp_paths();
    record(&paths, "Stop", json!({}));
    let path = state_path(&paths, SESSION).unwrap();
    assert!(path.exists());

    record(&paths, "SessionEnd", json!({ "reason": "exit" }));
    assert!(!path.exists());
    // A second SessionEnd (or one for a session never recorded) is fine.
    record(&paths, "SessionEnd", json!({ "reason": "exit" }));
}

#[test]
fn an_ignored_event_leaves_the_previous_state_alone() {
    let (_dir, paths) = temp_paths();
    record(&paths, "PermissionRequest", json!({ "tool_name": "Bash" }));
    record(
        &paths,
        "Notification",
        json!({ "notification_type": "auth_success" }),
    );
    let state = read_state(&state_path(&paths, SESSION).unwrap()).unwrap();
    assert_eq!(state.event, "PermissionRequest");
}

#[test]
fn session_start_sweeps_state_files_left_by_sessions_that_crashed() {
    let (_dir, paths) = temp_paths();
    let dir = hooks_dir(&paths);
    fs::create_dir_all(&dir).unwrap();
    let old = dir.join("dead-session.json");
    let recent = dir.join("live-session.json");
    fs::write(&old, "{}").unwrap();
    fs::write(&recent, "{}").unwrap();
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60))
        .unwrap();

    record(&paths, "SessionStart", json!({ "source": "startup" }));
    assert!(!old.exists(), "a week-old state survived");
    assert!(recent.exists(), "a live state was swept");
    assert!(state_path(&paths, SESSION).unwrap().exists());
}

#[test]
fn the_cache_rereads_a_file_only_when_it_changes() {
    let (_dir, paths) = temp_paths();
    let mut cache = HookStateCache::new();
    assert_eq!(cache.lookup(&paths, &[SESSION]), None);

    record(&paths, "PreToolUse", json!({ "tool_name": "Bash" }));
    let seen = cache
        .lookup(&paths, &["not-this-one", SESSION])
        .expect("found");
    assert_eq!(seen.event, "PreToolUse");

    record(&paths, "PermissionRequest", json!({ "tool_name": "Bash" }));
    // A different length is a change even inside one mtime tick.
    let seen = cache.lookup(&paths, &[SESSION]).expect("found");
    assert_eq!(seen.event, "PermissionRequest");

    record(&paths, "SessionEnd", json!({}));
    assert_eq!(cache.lookup(&paths, &[SESSION]), None);
    cache.sweep();
    cache.sweep();
    assert!(cache.entries.is_empty());
}

// --- fusion -------------------------------------------------------------------

fn iso(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn hook(
    event: &str,
    tool: Option<&str>,
    notification: Option<&str>,
    when: SystemTime,
) -> HookState {
    HookState {
        session_id: SESSION.to_string(),
        event: event.to_string(),
        tool_name: tool.map(str::to_string),
        notification_type: notification.map(str::to_string),
        timestamp: iso(when),
    }
}

fn base() -> SystemTime {
    SystemTime::from(now())
}

fn secs(n: u64) -> SystemTime {
    base() + Duration::from_secs(n)
}

/// A pending ordinary tool call, stamped `at`.
fn pending_tool(at: SystemTime) -> LastEntry {
    LastEntry {
        kind: EntryKind::Assistant,
        has_message: true,
        tool_uses: vec!["Bash".to_string()],
        activity_at: Some(iso(at)),
        ..LastEntry::default()
    }
}

#[test]
fn with_no_hook_state_the_transcript_decides() {
    let entry = pending_tool(secs(0));
    assert_eq!(
        session_status(Some(&entry), secs(0), None, secs(300)),
        SessionStatus::Working
    );
}

#[test]
fn a_permission_request_newer_than_the_transcript_is_awaiting() {
    // The one thing the transcript cannot see: the tool call and a prompt
    // look the same in the file.
    let entry = pending_tool(secs(0));
    let prompt = hook("PermissionRequest", Some("Bash"), None, secs(1));
    assert_eq!(
        session_status(Some(&entry), secs(1), Some(&prompt), secs(300)),
        SessionStatus::Awaiting
    );
    let notified = hook("Notification", None, Some("permission_prompt"), secs(7));
    assert_eq!(
        session_status(Some(&entry), secs(7), Some(&notified), secs(300)),
        SessionStatus::Awaiting
    );
}

#[test]
fn a_tie_goes_to_the_hook() {
    let entry = pending_tool(secs(5));
    let prompt = hook("PermissionRequest", Some("Bash"), None, secs(5));
    assert_eq!(
        session_status(Some(&entry), secs(5), Some(&prompt), secs(6)),
        SessionStatus::Awaiting
    );
}

#[test]
fn a_denial_supersedes_the_permission_request_without_any_hook() {
    // A denial fires no PostToolUse. The rejection and the interrupt marker
    // land in the transcript after the PermissionRequest, so the transcript
    // wins and reads idle.
    let prompt = hook("PermissionRequest", Some("Bash"), None, secs(1));
    let denied = LastEntry {
        kind: EntryKind::User,
        has_message: true,
        ends_turn: true,
        activity_at: Some(iso(secs(20))),
        ..LastEntry::default()
    };
    assert_eq!(
        session_status(Some(&denied), secs(20), Some(&prompt), secs(21)),
        SessionStatus::Idle
    );
}

#[test]
fn a_new_prompt_supersedes_a_stale_stop() {
    // Hooks partly installed: no UserPromptSubmit fired, so the Stop from the
    // last turn is the newest hook state. The prompt line is newer still.
    let stopped = hook("Stop", None, None, secs(0));
    let typed = LastEntry {
        kind: EntryKind::User,
        has_message: true,
        activity_at: Some(iso(secs(60))),
        ..LastEntry::default()
    };
    assert_eq!(
        session_status(Some(&typed), secs(60), Some(&stopped), secs(70)),
        SessionStatus::Working
    );
}

#[test]
fn an_approved_tool_is_working_again_on_post_tool_use() {
    let entry = pending_tool(secs(0));
    let done = hook("PostToolUse", Some("Bash"), None, secs(90));
    assert_eq!(
        session_status(Some(&entry), secs(90), Some(&done), secs(95)),
        SessionStatus::Working
    );
}

#[test]
fn stop_and_idle_notifications_read_idle() {
    let entry = pending_tool(secs(0));
    for state in [
        hook("Stop", None, None, secs(10)),
        hook("StopFailure", None, None, secs(10)),
        hook("Notification", None, Some("idle_prompt"), secs(10)),
    ] {
        assert_eq!(
            session_status(Some(&entry), secs(10), Some(&state), secs(11)),
            SessionStatus::Idle,
            "{}",
            state.event
        );
    }
}

#[test]
fn the_file_mtime_never_hides_a_hook_state() {
    // The hook result attachment touches the mtime right after the hook runs.
    // Fusion compares against the conversation only.
    let entry = pending_tool(secs(0));
    let prompt = hook("PermissionRequest", Some("Bash"), None, secs(1));
    let mtime_after_the_hook = secs(2);
    assert_eq!(
        session_status(Some(&entry), mtime_after_the_hook, Some(&prompt), secs(3)),
        SessionStatus::Awaiting
    );
}

#[test]
fn a_hook_state_with_no_say_or_no_readable_time_is_ignored() {
    let entry = pending_tool(secs(0));
    let unknown = hook("PreCompact", None, None, secs(5));
    let mut garbled = hook("PermissionRequest", Some("Bash"), None, secs(5));
    garbled.timestamp = "yesterday".to_string();
    for state in [unknown, garbled] {
        assert_eq!(
            session_status(Some(&entry), secs(5), Some(&state), secs(6)),
            SessionStatus::Working
        );
    }
}

#[test]
fn a_hook_state_speaks_for_a_transcript_with_no_conversation_yet() {
    let prompt = hook("UserPromptSubmit", None, None, secs(1));
    assert_eq!(
        session_status(None, secs(1), Some(&prompt), secs(2)),
        SessionStatus::Working
    );
}

#[test]
fn every_documented_dialog_notification_has_a_status() {
    // The Claude Code hooks reference lists these `notification_type` values.
    // A dialog that opens waits on the user, and a dialog that closes gives
    // Claude the turn back.
    for kind in [
        "permission_prompt",
        "elicitation_dialog",
        "elicitation_url_dialog",
        "agent_needs_input",
    ] {
        assert_eq!(
            status_for_event("Notification", None, Some(kind)),
            Some(SessionStatus::Awaiting),
            "{kind}"
        );
    }
    for kind in ["elicitation_complete", "elicitation_response"] {
        assert_eq!(
            status_for_event("Notification", None, Some(kind)),
            Some(SessionStatus::Working),
            "{kind}"
        );
    }
    // Account and quota notices say nothing about whose turn it is.
    for kind in ["auth_success", "agent_completed", "quota_auto_resume_fired"] {
        assert_eq!(
            status_for_event("Notification", None, Some(kind)),
            None,
            "{kind}"
        );
    }
}
