//! What must hold for every real transcript, plus a golden row per fixture.
//!
//! The invariants are the Node suite's; the golden table is new. Between them,
//! a Claude Code release that renames a field shows up as a specific failure
//! ("session-04 now yields 0 prompts") rather than a vague one.

use crate::transcript::fixtures::{name, transcripts};
use crate::transcript::{
    parse_conversation, parse_session_and_conversation, parse_session_file, MAX_PROMPTS,
};
use crate::types::MessageRole;

/// Pinned per fixture: session-id prefix, prompts kept, conversation turns, and
/// total output tokens. Produced by running the Node parser over these same
/// files, so this is parity with the implementation being replaced — not just
/// self-consistency.
const GOLDEN: &[(&str, &str, usize, usize, u64)] = &[
    ("session-01.jsonl", "06bbe2d6", 0, 73, 80_711),
    ("session-02.jsonl", "7bc4283c", 5, 60, 54_636),
    ("session-03.jsonl", "a00486a4", 3, 36, 54_675),
    ("session-04.jsonl", "90540e02", 1, 81, 152_153),
    ("session-05.jsonl", "0944e97b", 5, 75, 90_225),
    ("session-06.jsonl", "53adf627", 5, 61, 64_252),
    ("session-07.jsonl", "1f31f05d", 5, 59, 64_805),
    ("session-08.jsonl", "9b29ef2e", 5, 68, 92_266),
];

#[test]
fn every_fixture_parses_into_a_usable_session() {
    for path in transcripts() {
        let file = name(&path);
        let parsed = parse_session_file(&path).expect("fixture reads");

        assert!(
            !parsed.session_id.is_empty(),
            "{file}: a real transcript must name its session"
        );
        assert!(
            parsed.prompts.len() <= MAX_PROMPTS,
            "{file}: kept {} prompts, cap is {MAX_PROMPTS}",
            parsed.prompts.len()
        );
        for prompt in &parsed.prompts {
            assert!(
                !prompt.text.trim().is_empty(),
                "{file}: an empty prompt was kept"
            );
        }
        assert!(
            parsed.last_entry.is_some(),
            "{file}: the trailing entry drives the status machine and must exist"
        );
        assert!(
            !parsed.last_timestamp.is_empty(),
            "{file}: no timestamp anywhere in the window"
        );
        // Counters are u64, so "never negative" is a type guarantee here; what
        // is worth asserting is that a long session actually accumulated some.
        assert!(
            parsed.cumulative_usage.output_tokens > 0,
            "{file}: no output tokens accumulated"
        );
    }
}

#[test]
fn fixtures_match_the_values_the_node_parser_produced() {
    for path in transcripts() {
        let file = name(&path);
        let (_, id_prefix, prompts, messages, output_tokens) = *GOLDEN
            .iter()
            .find(|g| g.0 == file)
            .unwrap_or_else(|| panic!("{file} has no golden row — add one"));

        let parsed = parse_session_file(&path).expect("fixture reads");
        let conversation = parse_conversation(&path).expect("fixture reads");

        assert!(
            parsed.session_id.starts_with(id_prefix),
            "{file}: session id {} is not {id_prefix}…",
            parsed.session_id
        );
        assert_eq!(parsed.prompts.len(), prompts, "{file}: prompt count moved");
        assert_eq!(conversation.len(), messages, "{file}: turn count moved");
        assert_eq!(
            parsed.cumulative_usage.output_tokens, output_tokens,
            "{file}: accumulated output tokens moved"
        );
    }
}

#[test]
fn every_fixture_yields_a_renderable_conversation() {
    for path in transcripts() {
        let file = name(&path);
        let messages = parse_conversation(&path).expect("fixture reads");
        assert!(
            !messages.is_empty(),
            "{file}: a real transcript must yield at least one turn"
        );
        for message in &messages {
            assert!(
                !message.text.is_empty(),
                "{file}: an empty turn must not be emitted"
            );
            if message.role == MessageRole::User {
                assert!(
                    !message.has_tool_use,
                    "{file}: a user turn cannot hold a tool call"
                );
            }
        }
    }
}

#[test]
fn the_combined_parse_agrees_with_the_single_purpose_ones() {
    // The pane reads meta and conversation from one file read; that shortcut has
    // to produce exactly what the two separate parsers would.
    for path in transcripts() {
        let file = name(&path);
        let (meta, messages) = parse_session_and_conversation(&path).expect("fixture reads");
        let conversation = parse_conversation(&path).expect("fixture reads");
        let session = parse_session_file(&path).expect("fixture reads");

        let shape = |m: &[crate::types::ConversationMessage]| {
            m.iter().map(|m| (m.role, m.text.len())).collect::<Vec<_>>()
        };
        assert_eq!(
            shape(&messages),
            shape(&conversation),
            "{file}: combined parse drifted from parse_conversation"
        );
        assert_eq!(meta.session_id, session.session_id, "{file}: session id");
        assert_eq!(
            meta.last_timestamp, session.last_timestamp,
            "{file}: last timestamp"
        );
        assert_eq!(meta.last_entry, session.last_entry, "{file}: last entry");
    }
}
