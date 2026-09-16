//! AppleScript construction, all of it pure.
//!
//! The long-prompt cases are the important ones. A terminal's input queue holds
//! about 1KB (`MAX_INPUT`). Writing a whole ~1.3KB prompt in one call let the
//! line discipline discard the first 1024 bytes, and the trailing Return then
//! submitted only the tail that was left in the box. Three agents received
//! things like "ong assumption, a hidden business rule..." as their entire
//! revision instruction — one of them for a task belonging to a different
//! project.

use crate::term::{
    build_close_script, build_focus_script, build_launch_script, build_send_text_script,
    build_viewer_tab_script, escape_applescript, escape_shell_single, SEND_CHUNK_SIZE,
};

const TTY: &str = "/dev/ttys001";

/// Every `write text "…" newline NO` in the script, exactly as it appears
/// (still AppleScript-escaped). The bare `write text (ASCII character 13)` has
/// no quoted argument, so it is deliberately not one of these.
fn quoted_writes(script: &str) -> Vec<String> {
    const OPEN: &str = "write text \"";
    let mut out = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let mut value = String::new();
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in after.char_indices() {
            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => {
                    value.push(ch);
                    escaped = true;
                }
                '"' => {
                    end = Some(index);
                    break;
                }
                _ => value.push(ch),
            }
        }
        let Some(end) = end else { break };
        out.push(value);
        rest = &after[end + 1..];
    }
    out
}

fn unescape(value: &str) -> String {
    value.replace("\\\"", "\"").replace("\\\\", "\\")
}

#[test]
fn escapes_double_quotes_and_backslashes_for_applescript() {
    assert_eq!(
        escape_applescript("say \"hi\" \\ bye"),
        "say \\\"hi\\\" \\\\ bye"
    );
}

#[test]
fn escapes_single_quotes_for_the_shell() {
    assert_eq!(escape_shell_single("it's"), "it'\\''s");
}

#[test]
fn launch_opens_a_background_tab_and_restores_the_previous_one() {
    let script = build_launch_script("cd /r && claude");
    assert!(script.contains("create tab with default profile"));
    assert!(
        script.contains("select prevTab"),
        "must not steal focus from the dashboard"
    );
    assert!(script.contains("write text \"cd /r && claude\""));
}

#[test]
fn a_viewer_tab_runs_the_command_instead_of_typing_it() {
    // `write text` races the new tab's shell startup: characters land while zsh
    // is still printing its own prompt, so the line arrives corrupted — once as
    // "etmux new-session …", once wrapped in an extra quote that dropped the
    // shell into a `quote>` continuation and hung there. Running the command as
    // the session's program has no race in it.
    let script = build_viewer_tab_script("tmux attach -t x");
    assert!(script.contains("create tab with default profile command "));
    assert!(script.contains("/bin/sh -lc "));
    assert!(
        !script.contains("write text"),
        "the viewer tab must not type into a shell: {script}"
    );
    assert!(
        script.contains("activate"),
        "a tab you are meant to look at has to come forward"
    );
}

#[test]
fn a_quote_in_a_viewer_command_survives_both_layers_of_escaping() {
    // Shell-escaped first (`'` becomes `'\''`), then AppleScript-escaped, which
    // doubles that backslash. Getting either layer wrong drops the tab into a
    // `quote>` continuation.
    let script = build_viewer_tab_script("echo it's");
    assert!(script.contains("echo it'\\\\''s"), "{script}");
}

#[test]
fn focus_searches_by_tty_and_reports_not_found_rather_than_failing_silently() {
    let script = build_focus_script(TTY);
    assert!(script.contains("if tty of s is \"/dev/ttys001\""));
    assert!(script.contains("return \"NOT_FOUND\""));
    assert!(
        script.contains("activate"),
        "focus must bring iTerm2 forward"
    );
}

#[test]
fn iterm2_closes_the_session_on_the_right_tty() {
    let script = build_close_script("/dev/ttys004");
    assert!(script.contains("set targetTTY to \"/dev/ttys004\""));
    assert!(script.contains("tell s to close"));
    // It must find the pane by tty rather than closing the frontmost tab.
    assert!(script.contains("if tty of s is targetTTY then"));
    assert!(script.contains("return \"NOT_FOUND\""));
}

#[test]
fn iterm2_splits_the_text_into_writes_under_the_input_limit() {
    let prompt = "x".repeat(1303);
    let writes = quoted_writes(&build_send_text_script(TTY, &prompt));
    assert!(writes.len() > 1, "the prompt went out as a single write");
    for write in &writes {
        assert!(
            write.chars().count() <= SEND_CHUNK_SIZE,
            "a {}-char write can overrun the queue",
            write.chars().count()
        );
    }
}

#[test]
fn every_character_arrives_in_order() {
    let prompt = "x".repeat(1303);
    let writes = quoted_writes(&build_send_text_script(TTY, &prompt));
    assert_eq!(
        writes.concat(),
        prompt,
        "the text was altered or lost in chunking"
    );
}

#[test]
fn the_submitting_return_still_comes_last_once() {
    let prompt = "x".repeat(1303);
    let script = build_send_text_script(TTY, &prompt);
    assert_eq!(script.matches("ASCII character 13").count(), 1);
    assert!(
        script.rfind("ASCII character 13") > script.rfind("newline NO\n         delay 0.05"),
        "the Return must follow every chunk"
    );
}

#[test]
fn there_is_a_pause_between_chunks_and_before_the_return() {
    // Writing the text and the return in one go can beat the agent's input
    // handler and lose the submission.
    let script = build_send_text_script(TTY, &"x".repeat(1303));
    assert!(script.contains("delay 0.05"));
    assert!(script.contains("delay 0.5\n"));
}

#[test]
fn quotes_and_backslashes_survive_chunking() {
    let tricky = format!("say \"hi\" \\ {} \"end\"", "y".repeat(500));
    let writes = quoted_writes(&build_send_text_script(TTY, &tricky));
    let rebuilt: String = writes.iter().map(|w| unescape(w)).collect();
    assert_eq!(rebuilt, tricky);
}

#[test]
fn empty_text_produces_no_quoted_writes_at_all() {
    let script = build_send_text_script(TTY, "");
    assert!(quoted_writes(&script).is_empty());
    // The Return is still sent, which is what submitting an empty line means.
    assert_eq!(script.matches("ASCII character 13").count(), 1);
}
