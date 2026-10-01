//! The message Claude Code writes when an API call fails, as the last entry.

use std::io::Write;

use serde_json::json;

use crate::transcript::{parse_session_file, Entry};

fn api_error_line(kind: &str, text: &str) -> serde_json::Value {
    json!({
        "type": "assistant",
        "timestamp": "2026-09-30T12:00:05.000Z",
        "isApiErrorMessage": true,
        "error": kind,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
    })
}

fn parse(lines: &[serde_json::Value]) -> Option<crate::types::LastEntry> {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    parse_session_file(file.path()).unwrap().last_entry
}

#[test]
fn an_api_error_line_carries_its_kind_and_first_line() {
    let line = api_error_line(
        "rate_limit",
        "You've hit your session limit · resets 2:10pm (America/Chicago)\nmore",
    );
    let entry = Entry::parse_line(&line.to_string())
        .expect("a line")
        .last_entry();
    let error = entry.api_error.expect("an API error");
    assert_eq!(error.kind, "rate_limit");
    assert_eq!(
        error.text,
        "You've hit your session limit · resets 2:10pm (America/Chicago)"
    );
    assert_eq!(error.short(), "usage limit");

    let ordinary = json!({
        "type": "assistant",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }] },
    });
    assert!(Entry::parse_line(&ordinary.to_string())
        .expect("a line")
        .last_entry()
        .api_error
        .is_none());
}

/// Real transcripts follow the error with bookkeeping, or with the turn's
/// `turn_duration`. Neither may hide that the session stopped on an error.
#[test]
fn the_error_stays_the_newest_entry_through_what_follows_it() {
    let error = api_error_line("server_error", "Request timed out");
    for after in [
        vec![
            json!({ "type": "last-prompt" }),
            json!({ "type": "cost-state" }),
        ],
        vec![json!({
            "type": "system",
            "subtype": "turn_duration",
            "timestamp": "2026-09-30T12:00:06.000Z",
        })],
    ] {
        let mut lines = vec![error.clone()];
        lines.extend(after.clone());
        let last = parse(&lines).expect("a last entry");
        assert_eq!(
            last.api_error.map(|e| e.text).as_deref(),
            Some("Request timed out"),
            "hidden by {after:?}"
        );
    }
}

/// The person tells it to go on: the error is over.
#[test]
fn a_message_after_the_error_clears_it() {
    let last = parse(&[
        api_error_line("server_error", "Request timed out"),
        json!({
            "type": "user",
            "timestamp": "2026-09-30T12:01:00.000Z",
            "message": { "role": "user", "content": "go on" },
        }),
    ])
    .expect("a last entry");
    assert!(last.api_error.is_none());
}
