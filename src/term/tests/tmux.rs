//! tmux command construction, all of it pure.

use crate::term::{
    build_attach_shell_command, build_kill_pane_args, build_new_session_args,
    build_new_window_args, build_send_enter_args, build_send_text_args_chunked,
    build_viewer_session_name, list_panes_args, parse_pane_list, Pane, DETACHED_HEIGHT,
    DETACHED_WIDTH, SEND_CHUNK_SIZE,
};

#[test]
fn a_new_session_is_created_detached_so_the_dashboard_keeps_focus() {
    let args = build_new_session_args("cs", "/r", "run me", Some("task-1"));
    assert!(args.contains(&"-d".to_string()), "must be detached");
    assert_eq!(args[..6], ["new-session", "-d", "-s", "cs", "-c", "/r"]);
    assert_eq!(args[args.len() - 3..], ["/bin/sh", "-lc", "run me"]);
    assert!(args.contains(&"task-1".to_string()));
}

#[test]
fn a_detached_session_is_created_wider_than_the_tmux_default_80x24() {
    // 80 columns wraps QA reports and sets the width copied text breaks at.
    let args = build_new_session_args("cs", "/tmp", "x", None);
    let after = |flag: &str| {
        let at = args.iter().position(|a| a == flag).expect("flag");
        args[at + 1].parse::<u16>().expect("number")
    };
    assert!(
        after("-x") > 80,
        "width should exceed the 80-column default"
    );
    assert!(after("-y") > 24, "height should exceed the 24-row default");
    assert_eq!(after("-x"), DETACHED_WIDTH);
    assert_eq!(after("-y"), DETACHED_HEIGHT);
}

#[test]
fn a_second_launch_adds_a_window_to_the_existing_session() {
    let args = build_new_window_args("cs", "/r", "run me", None);
    assert_eq!(args[0], "new-window");
    assert!(args.contains(&"-d".to_string()));
    assert!(args.contains(&"cs:".to_string()));
    assert_eq!(args[args.len() - 3..], ["/bin/sh", "-lc", "run me"]);
}

#[test]
fn parses_list_panes_output_into_a_tty_index() {
    let map = parse_pane_list("/dev/ttys004\t%3\tclaude-sessions\t2\n/dev/ttys009\t%7\tother\t0\n");
    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get("/dev/ttys004"),
        Some(&Pane {
            pane_id: "%3".to_string(),
            session_name: "claude-sessions".to_string(),
            window_index: "2".to_string(),
        })
    );
}

#[test]
fn malformed_or_empty_pane_output_yields_an_empty_index() {
    assert!(parse_pane_list("").is_empty());
    assert!(parse_pane_list("garbage\n\n").is_empty());
    assert!(parse_pane_list("\t%3\tx\t0\n").is_empty());
}

#[test]
fn the_pane_format_asks_for_the_tty_first_because_that_is_the_join_key() {
    let args = list_panes_args();
    assert_eq!(args[..3], ["list-panes", "-a", "-F"]);
    assert!(args[3].starts_with("#{pane_tty}"));
}

#[test]
fn sends_one_send_keys_per_chunk_covering_the_whole_prompt() {
    let prompt = "x".repeat(1303);
    let calls = build_send_text_args_chunked("%3", &prompt);
    assert!(calls.len() > 1, "tmux still sends it in one call");
    for args in &calls {
        // `-l` sends the text literally, so a prompt containing `;` or `Enter`
        // is delivered as characters rather than read as a tmux key name.
        assert_eq!(args[..4], ["send-keys", "-t", "%3", "-l"]);
        assert!(args[4].chars().count() <= SEND_CHUNK_SIZE);
    }
    let sent: String = calls.iter().map(|args| args[4].clone()).collect();
    assert_eq!(sent, prompt);
}

#[test]
fn a_short_prompt_is_a_single_send_keys_and_an_empty_one_is_none() {
    assert_eq!(build_send_text_args_chunked("%3", "hello").len(), 1);
    assert!(build_send_text_args_chunked("%3", "").is_empty());
}

#[test]
fn the_submitting_return_is_its_own_call() {
    // Enter is a tmux key name, so it cannot ride along in a literal `-l` write.
    assert_eq!(
        build_send_enter_args("%3"),
        ["send-keys", "-t", "%3", "Enter"]
    );
}

#[test]
fn tmux_kills_the_pane_it_found() {
    assert_eq!(build_kill_pane_args("%7"), ["kill-pane", "-t", "%7"]);
}

// --- viewer tabs ------------------------------------------------------------
//
// `o` on a tmux-hosted session used to print "run: tmux attach -t …" and stop,
// which is not opening a session — it is homework. These cover the command that
// replaced it.

#[test]
fn the_viewer_session_is_named_per_window_not_per_session() {
    // Two tabs attached to the SAME tmux session share window selection, so
    // opening task A then task B would yank the first tab onto B.
    assert_ne!(
        build_viewer_session_name("claude-sessions", "0"),
        build_viewer_session_name("claude-sessions", "1")
    );
}

#[test]
fn selects_the_window_before_attaching_so_nothing_needs_escaping() {
    let cmd = build_attach_shell_command("claude-sessions", "3");
    let select = cmd.find("select-window").expect("select-window");
    // "attach" alone also matches inside "destroy-unattached".
    let attach = cmd.find("tmux attach").expect("attach");
    assert!(select < attach, "select-window must precede attach");
    // A literal backslash-semicolon is what would need escaping through both
    // AppleScript and the shell. Plain `;` is fine and is what is used.
    assert!(
        !cmd.contains("\\;"),
        "must not rely on a tmux separator: {cmd}"
    );
}

#[test]
fn groups_the_viewer_onto_the_real_session_rather_than_creating_a_new_one() {
    let cmd = build_attach_shell_command("claude-sessions", "0");
    assert!(cmd.contains("new-session -d -t 'claude-sessions'"), "{cmd}");
}

#[test]
fn never_sets_destroy_unattached_which_would_kill_the_viewer_before_it_attaches() {
    // The session is created detached, so destroy-unattached applies instantly
    // and tmux removes it on the spot. The attach then fails with "can't find
    // session: claude-sessions-view0" and iTerm2 raises "A session ended very
    // soon after starting". It cost the whole feature; the tidy-up is not worth
    // it, and an abandoned viewer holds no processes of its own.
    let cmd = build_attach_shell_command("claude-sessions", "0");
    assert!(!cmd.contains("destroy-unattached"), "{cmd}");
}

#[test]
fn reuses_an_existing_viewer_rather_than_failing_on_it() {
    // new-session errors when the name is taken, so the error is swallowed and
    // the following select-window + attach land on the viewer already there.
    let cmd = build_attach_shell_command("claude-sessions", "0");
    assert!(
        cmd.contains(
            "tmux new-session -d -t 'claude-sessions' -s 'claude-sessions-view0' 2>/dev/null"
        ),
        "{cmd}"
    );
}

#[test]
fn a_session_name_with_a_quote_in_it_cannot_break_out_of_the_shell_word() {
    let cmd = build_attach_shell_command("it's", "0");
    assert!(cmd.contains("'it'\\''s'"), "{cmd}");
}
