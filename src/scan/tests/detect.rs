//! Which processes count as a Claude session.
//!
//! Claude Code runs helper subcommands under the same binary name — prewarmed
//! PTY workers, its background daemon, MCP bridges — and creates and recycles
//! them on its own schedule. Counting those made sessions appear and disappear
//! in the dashboard with nobody touching the keyboard, which looked exactly
//! like the tool spawning work by itself.

use crate::scan::{
    is_daemon_scratch_cwd, is_helper_flag, is_interactive_claude, launch_task_id, session_id_flag,
};

#[test]
fn a_plain_claude_process_is_a_session() {
    assert!(is_interactive_claude("claude"));
    assert!(is_interactive_claude("/Users/k/.local/bin/claude"));
}

#[test]
fn prewarmed_workers_are_not_sessions() {
    // These are what appeared unbidden: bg-spare and bg-pty-host under
    // /tmp/cc-daemon-501/.../spare.
    assert!(!is_interactive_claude("claude bg-spare"));
    assert!(!is_interactive_claude("claude bg-pty-host"));
    assert!(!is_interactive_claude("claude bg-anything-else"));
}

#[test]
fn the_background_daemon_and_mcp_bridges_are_not_sessions() {
    assert!(!is_interactive_claude("claude daemon run"));
    assert!(!is_interactive_claude("claude mcp serve"));
}

#[test]
fn maintenance_subcommands_are_not_sessions() {
    for sub in [
        "update",
        "install",
        "doctor",
        "plugin",
        "config",
        "migrate-installer",
    ] {
        assert!(
            !is_interactive_claude(&format!("claude {sub}")),
            "claude {sub} counted as a session"
        );
    }
}

#[test]
fn this_tool_never_counts_itself() {
    assert!(!is_interactive_claude("node claude-sessions"));
    assert!(!is_interactive_claude("claude-sessionsd"));
}

#[test]
fn non_claude_processes_are_ignored() {
    assert!(!is_interactive_claude("node"));
    assert!(!is_interactive_claude(""));
}

#[test]
fn an_unfamiliar_subcommand_still_shows_up_rather_than_vanishing_silently() {
    // Deliberately a deny-list: a new interactive mode should be visible and
    // questioned, not hidden by an allow-list nobody remembers to update.
    assert!(is_interactive_claude("claude some-future-mode"));
    // A longer word that merely starts with a denied one is not that word.
    assert!(is_interactive_claude("claude plugins-ui"));
    assert!(is_interactive_claude("claude configure-me"));
}

#[test]
fn the_deny_list_needs_claude_on_a_word_boundary_of_its_own() {
    // `\bclaude\s+…` — a binary whose name merely ends in "claude" is not the
    // thing being denied.
    assert!(is_interactive_claude("myclaude daemon"));
}

#[test]
fn recognises_claude_codes_prewarm_scratch_area() {
    assert!(is_daemon_scratch_cwd(
        "/private/tmp/cc-daemon-501/f733b519/spare"
    ));
    assert!(is_daemon_scratch_cwd("/tmp/cc-daemon-501/abc/spare"));
}

#[test]
fn leaves_real_working_directories_alone() {
    assert!(!is_daemon_scratch_cwd("/Users/k/dev/app"));
    assert!(!is_daemon_scratch_cwd("/Users/k/dev/cc-daemon-notes"));
    assert!(!is_daemon_scratch_cwd("/tmp/cc-daemon-/spare"));
    assert!(!is_daemon_scratch_cwd("/tmp/cc-daemon-501"));
    assert!(!is_daemon_scratch_cwd(""));
}

const UUID: &str = "0198e4f0-1b3c-7a2d-9f4e-5c6b7a8d9e0f";

#[test]
fn a_declared_session_id_is_read_from_either_flag_and_either_spelling() {
    for argv in [
        format!("claude --resume {UUID}"),
        format!("claude --resume={UUID}"),
        format!("claude --session-id {UUID}"),
        format!("claude --session-id={UUID} --dangerously-skip-permissions"),
    ] {
        assert_eq!(session_id_flag(&argv).as_deref(), Some(UUID), "{argv}");
    }
}

#[test]
fn a_flag_without_a_full_session_id_declares_nothing() {
    assert_eq!(session_id_flag("claude --resume"), None);
    assert_eq!(session_id_flag("claude --resume abc"), None);
    assert_eq!(session_id_flag("claude --resume-later abc"), None);
    assert_eq!(session_id_flag("claude"), None);
}

#[test]
fn helpers_are_caught_in_their_flag_spelling_too() {
    // Invisible to `ps -o comm`, so a PTY host showed up as a session and
    // competed for a transcript with the real one.
    assert!(is_helper_flag("claude --bg-pty-host"));
    assert!(is_helper_flag("/usr/bin/claude --bg-spare --whatever"));
    assert!(!is_helper_flag("claude --background"));
    assert!(!is_helper_flag("claude"));
    // Not a flag if it is glued to something else.
    assert!(!is_helper_flag("claude--bg-spare"));
}

#[test]
fn the_launch_task_is_read_out_of_the_environment() {
    assert_eq!(
        launch_task_id("claude PATH=/usr/bin CLAUDE_SESSIONS_TASK_ID=6137 TERM=xterm"),
        Some(6137)
    );
    assert_eq!(launch_task_id("claude TERM=xterm"), None);
    assert_eq!(launch_task_id("claude CLAUDE_SESSIONS_TASK_ID="), None);
    // A near-miss variable is not the variable.
    assert_eq!(launch_task_id("claude MY_CLAUDE_SESSIONS_TASK_ID=1"), None);
}

#[test]
fn an_environment_value_is_never_read_as_a_command_line_flag() {
    // The reason argv and the environment are two separate `ps` calls: a prompt
    // sitting in the environment must not be able to name a transcript.
    let environ = format!("claude CLAUDE_SESSIONS_PROMPT=--resume {UUID}");
    assert_eq!(launch_task_id(&environ), None);
    // …and the value only reaches session_id_flag if somebody passes it there.
    assert_eq!(session_id_flag("claude"), None);
}
