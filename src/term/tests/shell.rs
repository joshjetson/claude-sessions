//! The pieces both drivers share: the shell line, the chunking, the tty.

use crate::term::{
    build_shell_command, chunk_text, normalize_tty, shell_quote, SessionRef, SEND_CHUNK_SIZE,
    TASK_ID_ENV,
};

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn builds_a_cd_export_and_command_line() {
    assert_eq!(
        build_shell_command("/repo", "claude --x", &env(&[(TASK_ID_ENV, "5944")])),
        "cd '/repo' && export CLAUDE_SESSIONS_TASK_ID='5944' && claude --x"
    );
}

#[test]
fn quotes_paths_containing_spaces_and_single_quotes() {
    assert_eq!(
        build_shell_command("/a/it's here/x y", "claude", &[]),
        "cd '/a/it'\\''s here/x y' && claude"
    );
}

#[test]
fn omits_the_export_clause_when_there_is_no_env() {
    assert_eq!(
        build_shell_command("/r", "claude", &[]),
        "cd '/r' && claude"
    );
}

#[test]
fn exports_keep_the_order_they_were_added_in() {
    assert_eq!(
        build_shell_command("/r", "claude", &env(&[("A", "1"), ("B", "2")])),
        "cd '/r' && export A='1' && export B='2' && claude"
    );
}

#[test]
fn one_builder_serves_both_drivers() {
    // The Node app carried a copy of this in each driver. They differed only in
    // whether the export clauses were joined before being joined with the rest,
    // which produces the same line — so there is one function here and this test
    // is what keeps that claim honest.
    let line = build_shell_command("/repo", "claude --x", &env(&[(TASK_ID_ENV, "5944")]));
    assert!(line.starts_with("cd '/repo' && export "));
    assert!(line.ends_with(" && claude --x"));
}

#[test]
fn a_quote_cannot_break_out_of_a_shell_word() {
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("plain"), "'plain'");
}

#[test]
fn a_short_prompt_is_still_a_single_write() {
    assert_eq!(chunk_text("hello", SEND_CHUNK_SIZE).len(), 1);
}

#[test]
fn empty_text_produces_no_writes_at_all() {
    assert!(chunk_text("", SEND_CHUNK_SIZE).is_empty());
}

#[test]
fn a_long_prompt_is_split_into_writes_under_the_input_limit() {
    // 1303 characters: the length of a real revision prompt, and the case where
    // a terminal's ~1KB input queue silently ate the first 1024 bytes.
    let prompt = "x".repeat(1303);
    let chunks = chunk_text(&prompt, SEND_CHUNK_SIZE);
    assert!(chunks.len() > 1, "the prompt went out as a single write");
    for chunk in &chunks {
        assert!(
            chunk.chars().count() <= SEND_CHUNK_SIZE,
            "a {}-char write can overrun the queue",
            chunk.chars().count()
        );
    }
    assert_eq!(chunks.concat(), prompt, "the text was altered in chunking");
}

#[test]
fn chunking_never_splits_a_multi_byte_character() {
    // The Node original sliced by UTF-16 code unit, which can cut a surrogate
    // pair in half. Chunking by character cannot.
    let text = "é".repeat(SEND_CHUNK_SIZE + 5);
    let chunks = chunk_text(&text, SEND_CHUNK_SIZE);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chars().count(), SEND_CHUNK_SIZE);
    assert_eq!(chunks.concat(), text);
}

#[test]
fn normalises_the_tty_form_ps_reports_to_the_one_the_terminals_report() {
    // This mapping is what lets a driver act on sessions the scanner found.
    assert_eq!(
        normalize_tty(Some("ttys004")).as_deref(),
        Some("/dev/ttys004")
    );
    assert_eq!(
        normalize_tty(Some("/dev/ttys004")).as_deref(),
        Some("/dev/ttys004")
    );
    assert_eq!(normalize_tty(Some("??")), None);
    assert_eq!(normalize_tty(Some("-")), None);
    assert_eq!(normalize_tty(Some("")), None);
    assert_eq!(normalize_tty(None), None);
}

#[test]
fn a_session_ref_normalises_what_the_scanner_recorded() {
    assert_eq!(
        SessionRef::from_tty("ttys004").tty_device().as_deref(),
        Some("/dev/ttys004")
    );
    assert_eq!(SessionRef::from_tty("??").tty_device(), None);
    assert_eq!(SessionRef::default().tty_device(), None);
}
