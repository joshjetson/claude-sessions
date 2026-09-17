//! tmux command construction, all of it pure.

use crate::term::{
    build_attach_shell_command, build_kill_pane_args, build_new_session_args,
    build_new_window_args, build_send_enter_args, build_send_text_args_chunked,
    build_viewer_session_name, list_panes_args, parse_client_list, parse_pane_list,
    parse_session_groups, pick_attached_group_session, Pane, DETACHED_HEIGHT, DETACHED_WIDTH,
    SEND_CHUNK_SIZE,
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
    let map =
        parse_pane_list("/dev/ttys004\t%3\tclaude-sessions\t2\t0\n/dev/ttys009\t%7\tother\t0\t0\n");
    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get("/dev/ttys004"),
        Some(&Pane {
            pane_id: "%3".to_string(),
            session_name: "claude-sessions".to_string(),
            window_index: "2".to_string(),
            dead: false,
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
    let args = list_panes_args("claude-sessions");
    // Scoped to ONE session, not `-a`. Every viewer is a grouped session sharing
    // the target's windows, so `-a` returned a copy of every pane per viewer and
    // the map kept a viewer's name — which is what made viewer names nest.
    assert_eq!(args[..4], ["list-panes", "-s", "-t", "claude-sessions"]);
    assert_eq!(args[4], "-F");
    assert!(args[5].starts_with("#{pane_tty}"));
    // And it asks whether the pane is dead: macOS reuses pty names, so one tty
    // can be held by both a live pane and a dead one.
    assert!(args[5].ends_with("#{pane_dead}"), "{}", args[5]);
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

#[test]
fn a_dead_pane_never_displaces_a_live_one_on_the_same_tty() {
    // macOS reuses pty device names, so one tty is held by both a live pane and
    // a finished one. Whichever printed last used to win — one keypress away
    // from opening a finished task's tab instead of the running one.
    let live_first = parse_pane_list("/dev/ttys004\t%3\tcs\t2\t0\n/dev/ttys004\t%9\tcs\t7\t1\n");
    assert_eq!(live_first.get("/dev/ttys004").unwrap().pane_id, "%3");

    // …and the other order, which is the one that used to break.
    let dead_first = parse_pane_list("/dev/ttys004\t%9\tcs\t7\t1\n/dev/ttys004\t%3\tcs\t2\t0\n");
    assert_eq!(dead_first.get("/dev/ttys004").unwrap().pane_id, "%3");
}

#[test]
fn the_attached_client_decides_which_session_is_driven() {
    // A viewer is a grouped session sharing the target's windows. Selecting a
    // window on the TARGET moves a session nobody is looking at.
    let clients = parse_client_list(
        "/dev/ttys010\tclaude-sessions-view0\t200\n/dev/ttys011\tunrelated\t300\n",
    );
    let groups = parse_session_groups(
        "claude-sessions\tcsgroup\nclaude-sessions-view0\tcsgroup\nunrelated\t\n",
    );
    assert_eq!(
        pick_attached_group_session(&clients, &groups, "claude-sessions").as_deref(),
        Some("claude-sessions-view0")
    );
}

#[test]
fn a_client_on_the_target_itself_wins_outright() {
    let clients = parse_client_list("/dev/ttys010\tclaude-sessions\t100\n");
    let groups = parse_session_groups("claude-sessions\tcsgroup\n");
    assert_eq!(
        pick_attached_group_session(&clients, &groups, "claude-sessions").as_deref(),
        Some("claude-sessions")
    );
}

#[test]
fn nothing_attached_means_nothing_to_drive() {
    // The caller then opens a viewer tab instead of silently selecting a window
    // nobody can see.
    let clients = parse_client_list("/dev/ttys010\tunrelated\t100\n");
    let groups = parse_session_groups("claude-sessions\tcsgroup\nunrelated\t\n");
    assert_eq!(
        pick_attached_group_session(&clients, &groups, "claude-sessions"),
        None
    );
}

#[test]
fn clients_come_back_newest_activity_first() {
    let clients = parse_client_list("/dev/a\ts1\t100\n/dev/b\ts2\t900\n/dev/c\ts3\t500\n");
    assert_eq!(
        clients
            .iter()
            .map(|c| c.session.as_str())
            .collect::<Vec<_>>(),
        ["s2", "s3", "s1"]
    );
}

#[test]
fn malformed_client_and_session_lines_are_skipped() {
    assert!(parse_client_list("").is_empty());
    assert!(parse_client_list("garbage\n\n").is_empty());
    assert!(parse_client_list("\ts1\t5\n").is_empty());
    assert!(parse_session_groups("").is_empty());
    assert!(parse_session_groups("\tg\n").is_empty());
}

#[test]
fn real_tmux_output_resolves_a_reused_tty_to_the_live_pane() {
    // Captured verbatim from a live server, where four panes had exited under
    // `remain-on-exit` and macOS had reused their pty names. ttys006 is held by
    // BOTH a live pane (%249, window 0) and a dead one (%3, window 3) — and the
    // dead one prints last, so it used to win.
    let map = parse_pane_list(
        "/dev/ttys006\t%249\tclaude-sessions\t0\t0\n\
         /dev/ttys003\t%1\tclaude-sessions\t1\t1\n\
         /dev/ttys005\t%2\tclaude-sessions\t2\t1\n\
         /dev/ttys006\t%3\tclaude-sessions\t3\t1\n\
         /dev/ttys007\t%250\tclaude-sessions\t4\t0\n\
         /dev/ttys008\t%251\tclaude-sessions\t5\t0\n",
    );

    let six = map.get("/dev/ttys006").expect("ttys006");
    assert_eq!(six.pane_id, "%249", "the dead pane displaced the live one");
    assert_eq!(six.window_index, "0");
    assert!(!six.dead);

    // The ttys that only ever held a dead pane still resolve — there is nothing
    // better to offer, and dropping them would make focus fail with "no pane"
    // rather than opening the window the session ended in.
    assert!(map.get("/dev/ttys003").unwrap().dead);
    assert_eq!(map.len(), 5);
}
