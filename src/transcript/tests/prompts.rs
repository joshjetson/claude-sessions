//! The prompt filter, usage accumulation, and the tail window — the parts of a
//! session row that come from folding a transcript rather than rendering one.

use serde_json::json;

use super::Transcript;
use super::{assistant_text, assistant_text_with_usage, line, user_prompt, user_prompt_at};
use crate::transcript::{
    parse_conversation, parse_session_file, Entry, INITIAL_TAIL_SIZE, MAX_PROMPTS,
};

fn prompts(body: &str) -> Vec<String> {
    let t = Transcript::new(body);
    parse_session_file(&t.path)
        .expect("parse")
        .prompts
        .into_iter()
        .map(|p| p.text)
        .collect()
}

#[test]
fn unparseable_lines_are_skipped_not_thrown() {
    let body = format!(
        "{}{}{}{}{}{}",
        user_prompt("first question"),
        "{not json at all\n",
        "\n",
        "   \n",
        assistant_text("an answer"),
        user_prompt("second question"),
    );
    assert_eq!(prompts(&body), ["first question", "second question"]);
}

#[test]
fn an_empty_file_yields_an_empty_result() {
    let t = Transcript::new("");
    let parsed = parse_session_file(&t.path).expect("parse");
    assert_eq!(parsed.session_id, "");
    assert!(parsed.prompts.is_empty());
    assert!(parsed.last_entry.is_none());
    assert!(parse_conversation(&t.path).expect("parse").is_empty());
}

#[test]
fn a_file_of_only_ignored_entry_types_yields_nothing_at_all() {
    let body = format!(
        "{}{}{}",
        line(json!({ "type": "mode", "mode": "default", "sessionId": "s" })),
        line(json!({ "type": "file-history-snapshot", "messageId": "m" })),
        line(json!({ "type": "pr-link", "url": "x" })),
    );
    let t = Transcript::new(&body);
    assert!(parse_conversation(&t.path).expect("parse").is_empty());
    let parsed = parse_session_file(&t.path).expect("parse");
    assert!(parsed.prompts.is_empty());
    // Bookkeeping never drives the status, so there is no trailing entry for
    // the status machine to read, and it reports idle for having none.
    assert!(parsed.last_entry.is_none());
}

#[test]
fn bookkeeping_after_the_conversation_leaves_the_trailing_entry_alone() {
    // The shape every tool call leaves when PreToolUse / PostToolUse hooks are
    // installed: the tool call, then a hook result, a token reminder and the
    // mode lines. The tool call must stay the entry the status machine reads.
    let body = format!(
        "{}{}{}{}",
        line(json!({
            "type": "assistant",
            "timestamp": "2026-08-01T10:00:00.000Z",
            "message": { "role": "assistant", "content": [
                { "type": "tool_use", "name": "Bash", "input": {} }
            ]}
        })),
        line(json!({
            "type": "attachment",
            "timestamp": "2026-08-01T10:00:00.300Z",
            "attachment": { "type": "hook_success" }
        })),
        line(json!({ "type": "last-prompt" })),
        line(json!({ "type": "permission-mode", "permissionMode": "default" })),
    );
    let t = Transcript::new(&body);
    let parsed = parse_session_file(&t.path).expect("parse");
    let last = parsed.last_entry.expect("last entry");
    assert_eq!(last.kind.as_str(), "assistant");
    assert_eq!(last.tool_uses, vec!["Bash".to_string()]);
    // The attachment's later stamp is not conversational activity.
    assert_eq!(
        last.activity_at.as_deref(),
        Some("2026-08-01T10:00:00.000Z")
    );
    // The session's own "last active" stamp does include it.
    assert_eq!(parsed.last_timestamp, "2026-08-01T10:00:00.300Z");
}

#[test]
fn activity_is_the_newest_stamp_not_the_last_one_read() {
    // Real transcripts write a tool result, then a meta line stamped earlier.
    let body = format!(
        "{}{}",
        line(json!({
            "type": "user",
            "timestamp": "2026-08-01T10:00:57.609Z",
            "toolUseResult": { "ok": true },
            "message": { "role": "user", "content": [
                { "type": "tool_result", "content": "ok" }
            ]}
        })),
        line(json!({
            "type": "user",
            "isMeta": true,
            "timestamp": "2026-08-01T10:00:57.395Z",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "Base directory for this skill: /x" }
            ]}
        })),
    );
    let t = Transcript::new(&body);
    let parsed = parse_session_file(&t.path).expect("parse");
    let last = parsed.last_entry.expect("last entry");
    // The meta line is the newest conversational entry by position ...
    assert!(!last.has_tool_result);
    // ... and the activity clock keeps the newer stamp.
    assert_eq!(
        last.activity_at.as_deref(),
        Some("2026-08-01T10:00:57.609Z")
    );
    assert_eq!(parsed.last_timestamp, "2026-08-01T10:00:57.609Z");
}

#[test]
fn tool_results_and_sidechain_echoes_are_not_prompts() {
    let body = format!(
        "{}{}{}{}",
        user_prompt("real human prompt"),
        user_prompt_at(
            "file contents here",
            "2026-08-01T10:00:05.000Z",
            json!({ "toolUseResult": { "ok": true } })
        ),
        user_prompt_at(
            "agent echo",
            "2026-08-01T10:00:06.000Z",
            json!({ "sourceToolAssistantUUID": "abc" })
        ),
        line(json!({
            "type": "user", "userType": "internal",
            "message": { "role": "user", "content": "internal" },
        })),
    );
    assert_eq!(prompts(&body), ["real human prompt"]);
    let t = Transcript::new(&body);
    let texts: Vec<String> = parse_conversation(&t.path)
        .expect("parse")
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(texts, ["real human prompt"]);
}

#[test]
fn a_falsy_tool_use_result_still_counts_as_a_prompt() {
    // Node tested `entry.toolUseResult` for truthiness, so a null or empty value
    // never marked a line as tool output. Treating "the key exists" as the test
    // would silently lose real prompts.
    let body = format!(
        "{}{}",
        user_prompt_at(
            "kept",
            "2026-08-01T10:00:00.000Z",
            json!({ "toolUseResult": null })
        ),
        user_prompt_at(
            "also kept",
            "2026-08-01T10:00:01.000Z",
            json!({ "toolUseResult": "", "sourceToolAssistantUUID": "" })
        ),
    );
    assert_eq!(prompts(&body), ["kept", "also kept"]);
}

#[test]
fn interrupt_markers_and_system_authored_turns_are_filtered() {
    let body = format!(
        "{}{}{}",
        user_prompt("[Request interrupted by user]"),
        user_prompt("The user doesn't want to proceed"),
        user_prompt("genuine prompt"),
    );
    assert_eq!(prompts(&body), ["genuine prompt"]);
}

#[test]
fn only_the_last_n_prompts_are_kept() {
    let body: String = (1..=MAX_PROMPTS + 4)
        .map(|i| user_prompt(&format!("prompt {i}")))
        .collect();
    let kept = prompts(&body);
    assert_eq!(kept.len(), MAX_PROMPTS);
    assert_eq!(kept.first().unwrap(), "prompt 5");
    assert_eq!(kept.last().unwrap(), &format!("prompt {}", MAX_PROMPTS + 4));
}

#[test]
fn usage_accumulates_while_last_usage_stays_the_newest() {
    let body = format!(
        "{}{}{}",
        assistant_text_with_usage(
            "a",
            json!({ "input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 100 })
        ),
        assistant_text_with_usage(
            "b",
            json!({ "input_tokens": 20, "output_tokens": 2, "cache_creation_input_tokens": 50 })
        ),
        user_prompt("keeps the tail from growing"),
    );
    let t = Transcript::new(&body);
    let parsed = parse_session_file(&t.path).expect("parse");
    assert_eq!(parsed.cumulative_usage.input_tokens, 30);
    assert_eq!(parsed.cumulative_usage.output_tokens, 3);
    assert_eq!(parsed.cumulative_usage.cache_read_input_tokens, 100);
    assert_eq!(parsed.cumulative_usage.cache_creation_input_tokens, 50);
    assert_eq!(
        parsed.last_usage.expect("last usage").input_tokens,
        Some(20)
    );
}

#[test]
fn session_id_is_the_first_seen_while_branch_and_timestamp_are_the_newest() {
    let body = format!(
        "{}{}",
        line(json!({
            "type": "user", "userType": "external", "sessionId": "first",
            "gitBranch": "main", "timestamp": "2026-08-01T10:00:00.000Z",
            "message": { "role": "user", "content": "hello" },
        })),
        line(json!({
            "type": "user", "userType": "external", "sessionId": "second",
            "gitBranch": "feature", "timestamp": "2026-08-01T11:00:00.000Z",
            "message": { "role": "user", "content": "again" },
        })),
    );
    let t = Transcript::new(&body);
    let parsed = parse_session_file(&t.path).expect("parse");
    assert_eq!(parsed.session_id, "first");
    assert_eq!(parsed.git_branch, "feature");
    assert_eq!(parsed.last_timestamp, "2026-08-01T11:00:00.000Z");
}

// --- tolerance --------------------------------------------------------------

#[test]
fn a_field_with_an_unexpected_type_costs_that_field_only() {
    // The failure this guards against is silent: an entry dropped for one odd
    // field would take the trailing entry with it and park a live session at
    // "idle".
    let entry = Entry::parse_line(
        &json!({
            "type": "user", "userType": "external", "timestamp": 12345,
            "gitBranch": ["not", "a", "string"], "data": "not an object",
            "message": { "role": "user", "content": "still readable" },
        })
        .to_string(),
    )
    .expect("entry parses");
    assert_eq!(entry.human_prompt(), Some("still readable"));
    assert_eq!(entry.timestamp(), "");
    assert_eq!(entry.git_branch(), None);
    assert!(entry.data.is_none());
}

#[test]
fn an_unknown_entry_type_is_carried_through_rather_than_dropped() {
    let entry = Entry::parse_line(r#"{"type":"some-future-type","x":1}"#).expect("entry parses");
    assert_eq!(entry.last_entry().kind.as_str(), "some-future-type");
    assert!(Entry::parse_line("not json").is_none());
    assert!(Entry::parse_line("   ").is_none());
}

// --- tail reading -----------------------------------------------------------

/// A body larger than the initial window, with its only prompt at the head.
fn big_body(prompt: &str) -> String {
    let mut body = user_prompt(prompt);
    let filler = assistant_text(&"x".repeat(4000));
    while body.len() < (INITIAL_TAIL_SIZE as usize) + 8192 {
        body.push_str(&filler);
    }
    body
}

#[test]
fn a_partial_first_line_from_a_mid_file_offset_is_discarded() {
    let t = Transcript::new(&big_body("prompt at the very top"));
    // The 512 KB window starts mid-line; the fragment must be dropped rather
    // than half-parsed into a corrupt turn.
    let messages = parse_conversation(&t.path).expect("parse");
    assert!(messages.iter().all(|m| !m.text.is_empty()));
    assert!(messages.iter().all(|m| m.text.starts_with('x')));
}

#[test]
fn the_window_grows_until_the_prompts_are_found() {
    let t = Transcript::new(&big_body("the only prompt, at the head"));
    // A single 512 KB tail read would miss it entirely.
    let parsed = parse_session_file(&t.path).expect("parse");
    assert_eq!(
        parsed.prompts.iter().map(|p| &p.text).collect::<Vec<_>>(),
        ["the only prompt, at the head"]
    );
}

#[test]
fn a_last_line_without_its_newline_is_still_read() {
    // Node split the whole window on newlines and tried every piece, so a file
    // whose final line has no terminator still contributed it.
    let body = user_prompt("terminated")
        + r#"{"type":"user","userType":"external","message":{"role":"user","content":"unterminated"}}"#;
    assert_eq!(prompts(&body), ["terminated", "unterminated"]);
}
