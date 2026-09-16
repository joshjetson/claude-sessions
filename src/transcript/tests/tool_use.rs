//! Every tool rendering the conversation pane can produce. These strings are
//! contracts: `test/parser.test.js` pinned each one, and a reader learns to scan
//! for them.

use serde_json::json;

use super::{assistant_tool, Transcript};
use crate::transcript::parse_conversation;

/// Render a tool call the way the pane would — through a real transcript, so the
/// block model is exercised rather than bypassed.
fn render(name: &str, input: serde_json::Value) -> String {
    let t = Transcript::new(&assistant_tool(name, input));
    parse_conversation(&t.path).expect("parse")[0].text.clone()
}

/// The same, from literal JSON text. `serde_json::json!` sorts object keys, so
/// this is the only way a test can say what *order* the transcript wrote the
/// tool arguments in.
fn render_raw(name: &str, input: &str) -> String {
    let body = format!(
        concat!(
            r#"{{"type":"assistant","timestamp":"2026-08-01T10:00:02.000Z","#,
            r#""message":{{"role":"assistant","content":"#,
            r#"[{{"type":"tool_use","name":"{name}","input":{input}}}]}}}}"#,
            "\n"
        ),
        name = name,
        input = input,
    );
    let t = Transcript::new(&body);
    parse_conversation(&t.path).expect("parse")[0].text.clone()
}

#[test]
fn known_tools_render_with_their_salient_argument() {
    assert_eq!(
        render("Read", json!({ "file_path": "/a/b/c/file.js" })),
        "[Read: c/file.js]"
    );
    assert_eq!(
        render("Write", json!({ "file_path": "/a/b/c/out.txt" })),
        "[Write: c/out.txt]"
    );
    assert_eq!(
        render("Bash", json!({ "command": "npm test" })),
        "[Bash: npm test]"
    );
    assert_eq!(
        render("Glob", json!({ "pattern": "**/*.ts" })),
        "[Glob: **/*.ts]"
    );
    assert_eq!(
        render("Grep", json!({ "pattern": "foo", "path": "/x/y" })),
        "[Grep: \"foo\" in x/y]"
    );
    assert_eq!(
        render("WebSearch", json!({ "query": "ratatui widgets" })),
        "[Search: \"ratatui widgets\"]"
    );
    assert_eq!(
        render("WebFetch", json!({ "url": "https://example.com/x" })),
        "[Fetch: https://example.com/x]"
    );
    assert_eq!(
        render("Task", json!({ "description": "find bugs" })),
        "[Task: find bugs]"
    );
    assert_eq!(render("TodoWrite", json!({})), "[TodoWrite]");
}

#[test]
fn missing_arguments_render_empty_rather_than_failing() {
    assert_eq!(render("Read", json!({})), "[Read: ]");
    assert_eq!(render("Bash", json!({})), "[Bash]");
    assert_eq!(render("Glob", json!({})), "[Glob: ]");
    assert_eq!(render("Grep", json!({})), "[Grep: \"\"]");
    // Task names its agent when it has no description, and says "agent" when it
    // has neither.
    assert_eq!(
        render("Task", json!({ "subagent_type": "explorer" })),
        "[Task: explorer]"
    );
    assert_eq!(render("Task", json!({})), "[Task: agent]");
}

#[test]
fn long_bash_commands_are_truncated_to_a_single_line() {
    let out = render(
        "Bash",
        json!({ "command": format!("echo {}", "z".repeat(200)) }),
    );
    assert!(out.chars().count() < 100, "not truncated: {out}");
    assert!(out.ends_with("...]"));
    assert!(!out.contains('\n'));
    // 80 characters of command, then the ellipsis: "[Bash: " + 77 + "..." + "]".
    assert_eq!(out.chars().count(), 7 + 77 + 3 + 1);
}

#[test]
fn edit_renders_a_plus_minus_diff_body() {
    let out = render(
        "Edit",
        json!({ "file_path": "/a/b.js", "old_string": "one\ntwo", "new_string": "three" }),
    );
    assert_eq!(
        out.split('\n').collect::<Vec<_>>(),
        ["[Edit: a/b.js]", "- one", "- two", "+ three"]
    );
}

#[test]
fn edit_with_no_strings_is_just_its_heading() {
    assert_eq!(
        render("Edit", json!({ "file_path": "/a/b.js" })),
        "[Edit: a/b.js]"
    );
    // An insertion has no removed side, and vice versa.
    assert_eq!(
        render(
            "Edit",
            json!({ "file_path": "/a/b.js", "new_string": "added" })
        ),
        "[Edit: a/b.js]\n+ added"
    );
}

#[test]
fn ask_user_question_surfaces_the_question_text() {
    assert_eq!(
        render(
            "AskUserQuestion",
            json!({ "questions": [{ "question": "Which database?" }] })
        ),
        "[Question: Which database?]"
    );
    assert_eq!(render("AskUserQuestion", json!({})), "[AskUserQuestion]");
    assert_eq!(
        render("AskUserQuestion", json!({ "questions": [] })),
        "[AskUserQuestion]"
    );
    // Long questions keep 60 characters and gain an ellipsis — a different
    // budget from Bash's 80/77 and the fallback's 60/57, all three preserved.
    let long = "q".repeat(70);
    let out = render(
        "AskUserQuestion",
        json!({ "questions": [{ "question": long }] }),
    );
    assert_eq!(out, format!("[Question: {}...]", "q".repeat(60)));
}

#[test]
fn unknown_and_mcp_tools_fall_back_to_name_plus_first_string_argument() {
    assert_eq!(
        render(
            "mcp__odoo__execute_method",
            json!({ "model": "project.task" })
        ),
        "[mcp__odoo__execute_method: project.task]"
    );
    assert_eq!(render("SomeFutureTool", json!({})), "[SomeFutureTool]");
    // Non-string arguments must not render as debug output.
    assert_eq!(
        render(
            "SomeFutureTool",
            json!({ "count": 3, "nested": { "a": 1 } })
        ),
        "[SomeFutureTool]"
    );
    // The *first* string argument, in transcript order, not alphabetical order:
    // sorting the keys would make every Odoo call advertise its method instead
    // of its model.
    assert_eq!(
        render_raw(
            "mcp__server__tool",
            r#"{"model":"project.task","method":"search_read"}"#
        ),
        "[mcp__server__tool: project.task]"
    );
    assert_eq!(
        render_raw(
            "mcp__server__tool",
            r#"{"zeta":"written first","alpha":"written second"}"#
        ),
        "[mcp__server__tool: written first]"
    );
    let long = "v".repeat(70);
    assert_eq!(
        render("SomeFutureTool", json!({ "arg": long })),
        format!("[SomeFutureTool: {}...]", "v".repeat(57))
    );
}

#[test]
fn a_nameless_tool_call_renders_nothing_but_still_counts_as_one() {
    let body = super::line(json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:00.000Z",
        "message": { "role": "assistant", "content": [
            { "type": "text", "text": "thinking about it" },
            { "type": "tool_use", "input": { "a": "b" } },
        ] },
    }));
    let t = Transcript::new(&body);
    let turn = &parse_conversation(&t.path).expect("parse")[0];
    assert_eq!(turn.text, "thinking about it");
    // Not rendered (Node required a string name to render), but the status
    // machine must still see a pending tool call.
    assert!(!turn.has_tool_use);
    let parsed = crate::transcript::parse_session_file(&t.path).expect("parse");
    assert!(parsed.last_entry.expect("last entry").has_tool_use());
}

#[test]
fn paths_shorten_to_their_last_two_segments() {
    assert_eq!(
        render("Read", json!({ "file_path": "file.js" })),
        "[Read: file.js]"
    );
    assert_eq!(
        render("Read", json!({ "file_path": "/file.js" })),
        "[Read: /file.js]"
    );
    assert_eq!(
        render("Read", json!({ "file_path": "/deep/nest/of/dirs/file.js" })),
        "[Read: dirs/file.js]"
    );
}
