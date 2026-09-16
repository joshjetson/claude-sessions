//! Building the turns the conversation pane renders.

use serde_json::json;

use super::{line, Transcript};
use crate::transcript::{extract_text_content, parse_conversation, Content, ContentBlock};
use crate::types::MessageRole;

fn turns(body: &str) -> Vec<crate::types::ConversationMessage> {
    let t = Transcript::new(body);
    parse_conversation(&t.path).expect("parse")
}

#[test]
fn a_turn_mixing_prose_and_tool_calls_keeps_both_and_flags_it() {
    let body = line(json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:00.000Z",
        "message": { "role": "assistant", "content": [
            { "type": "text", "text": "Let me look." },
            { "type": "tool_use", "name": "Read", "input": { "file_path": "/a/b.js" } },
        ] },
    }));
    let [turn] = &turns(&body)[..] else {
        panic!("expected one turn")
    };
    assert_eq!(turn.text, "Let me look.\n[Read: a/b.js]");
    assert!(turn.has_tool_use);
    assert_eq!(turn.role, MessageRole::Assistant);
}

#[test]
fn string_content_assistant_messages_still_parse() {
    let body = line(json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:00.000Z",
        "message": { "role": "assistant", "content": "plain string reply" },
    }));
    assert_eq!(turns(&body)[0].text, "plain string reply");
}

#[test]
fn turns_that_render_to_nothing_are_dropped() {
    // A thinking-only turn, and an assistant entry with no content at all.
    let body = format!(
        "{}{}{}",
        line(json!({
            "type": "assistant",
            "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "…", "signature": "s" },
            ] },
        })),
        line(json!({ "type": "assistant", "message": { "role": "assistant" } })),
        line(json!({
            "type": "assistant",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": "" }] },
        })),
    );
    assert!(turns(&body).is_empty());
}

#[test]
fn an_assistant_entry_without_the_assistant_role_is_not_a_turn() {
    // The role guard is what keeps a sub-agent's echo out of the pane.
    let body = line(json!({
        "type": "assistant",
        "message": { "role": "user", "content": "echoed" },
    }));
    assert!(turns(&body).is_empty());
}

#[test]
fn usage_rides_along_with_the_assistant_turn() {
    let body = line(json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:00.000Z",
        "message": {
            "role": "assistant", "content": "hi",
            "usage": { "input_tokens": 7, "output_tokens": 3 },
        },
    }));
    let usage = turns(&body)[0].usage.expect("usage carried");
    assert_eq!(usage.input_tokens, Some(7));
    assert_eq!(usage.output_tokens, Some(3));
}

// --- extractTextContent -----------------------------------------------------

fn content(value: serde_json::Value) -> Content {
    serde_json::from_value(value).expect("content")
}

#[test]
fn text_extraction_takes_the_first_thing_it_recognises() {
    assert_eq!(
        extract_text_content(&content(json!("bare string"))),
        "bare string"
    );
    assert_eq!(
        extract_text_content(&content(json!([{ "type": "text", "text": "block" }]))),
        "block"
    );
    // A tool result's string payload counts — that is how a tool-output user
    // line gets any text at all.
    assert_eq!(
        extract_text_content(&content(
            json!([{ "type": "tool_result", "content": "output" }])
        )),
        "output"
    );
    // A bare string element of the array is returned as-is.
    assert_eq!(extract_text_content(&content(json!(["loose"]))), "loose");
}

#[test]
fn text_extraction_skips_what_it_cannot_read_and_stops_at_the_first_match() {
    assert_eq!(extract_text_content(&content(json!([]))), "");
    assert_eq!(extract_text_content(&content(json!(42))), "");
    assert_eq!(
        extract_text_content(&content(json!([
            { "type": "thinking", "thinking": "hidden" },
            { "type": "text", "text": 5 },
            { "type": "tool_result", "content": { "not": "a string" } },
            { "type": "text", "text": "found" },
        ]))),
        "found"
    );
    // An empty first match wins — callers decide what an empty prompt means.
    assert_eq!(
        extract_text_content(&content(json!([
            { "type": "text", "text": "" },
            { "type": "text", "text": "later" },
        ]))),
        ""
    );
}

#[test]
fn a_tool_use_block_keeps_its_input_key_order() {
    // Sorted keys would make the fallback tool rendering advertise the wrong
    // argument; this is the shape that guarantee rests on. Parsed from literal
    // text because `serde_json::json!` would sort the keys first.
    let Content::Blocks(blocks) = serde_json::from_str::<Content>(
        r#"[{"type":"tool_use","name":"mcp__x__call","input":{"zeta":"last","alpha":"first"}}]"#,
    )
    .expect("content") else {
        panic!("expected blocks")
    };
    let ContentBlock::ToolUse { name, input } = &blocks[0] else {
        panic!("expected a tool use")
    };
    assert_eq!(name.as_deref(), Some("mcp__x__call"));
    assert_eq!(
        crate::transcript::format_tool_use("mcp__x__call", input),
        "[mcp__x__call: last]"
    );
}
