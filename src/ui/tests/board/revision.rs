//! Ported from `test/revision-into-session.test.js` and
//! `test/resume-conversation.test.js`.
//!
//! Resuming a live session's id in a new tab starts a second process against
//! the same conversation — the pile-up that once had nine agents racing on one
//! task. When a session is open, the revision is typed into it instead.
//!
//! `C` is the other case entirely: it reopens the conversation so you can ask
//! it something yourself, so it sends NO prompt and does NOT move the task to
//! In Progress. Both are asserted at the action layer, where the decision is
//! made, rather than by reading the source the way the Node suite had to.

use crate::ui::board::{start, LaunchKind};
use crate::ui::state::Action;

use super::fixtures::*;

fn revision() -> LaunchKind {
    LaunchKind::Revision {
        session_id: String::new(),
    }
}

fn conversation() -> LaunchKind {
    LaunchKind::Conversation {
        session_id: String::new(),
    }
}

#[test]
fn a_live_session_is_typed_into_not_resumed_in_a_new_tab() {
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![live_session("live-abc12345", Some(7777), 1000)],
    );
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let queued = actions(&mut state);
    let sent = queued.iter().find_map(|action| match action {
        Action::SendToSession(spec) => Some(spec),
        _ => None,
    });
    let spec = sent.expect("expected the revision to be sent into the live session");
    assert_eq!(spec.session_id, "live-abc12345");
    assert!(
        !queued.iter().any(|a| matches!(a, Action::Resume(_))),
        "it resumed into a new tab instead: {queued:?}"
    );
}

#[test]
fn the_revision_sent_into_a_session_is_the_revision_pipelines_prompt() {
    let (_dir, mut state) = board_state();
    with_sessions(&mut state, vec![live_session("live", Some(7777), 1000)]);
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let queued = actions(&mut state);
    let Some(Action::SendToSession(spec)) = queued
        .into_iter()
        .find(|a| matches!(a, Action::SendToSession(_)))
    else {
        panic!("no send");
    };
    // The same text a resumed session would have been launched with.
    let expected = crate::pipeline::resolve_pipeline("revision", None)
        .unwrap()
        .build_prompt(&crate::pipeline::PromptVars::new(7777, String::new()));
    assert!(
        spec.prompt.starts_with(expected.split(' ').next().unwrap()),
        "{}",
        spec.prompt
    );
    assert!(spec.prompt.contains("7777"), "{}", spec.prompt);
}

#[test]
fn with_no_live_session_it_falls_back_to_the_archive() {
    let (_dir, mut state) = board_state();
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let queued = actions(&mut state);
    assert!(
        queued.iter().any(|a| matches!(a, Action::Resume(_))),
        "{queued:?}"
    );
}

#[test]
fn a_live_session_with_no_tty_cannot_be_typed_into_so_it_uses_a_tab() {
    let (_dir, mut state) = board_state();
    let mut detached = live_session("live", Some(7777), 1000);
    detached.tty = Some("??".to_string());
    with_sessions(&mut state, vec![detached]);
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let flash = state.flash.clone().unwrap_or_default();
    assert!(flash.contains("no terminal to type into"), "{flash}");
    let queued = actions(&mut state);
    assert!(queued.iter().any(|a| matches!(a, Action::Resume(_))));
    assert!(!queued.iter().any(|a| matches!(a, Action::SendToSession(_))));
}

#[test]
fn the_most_recent_session_wins_when_several_are_live() {
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![
            live_session("older-one", Some(7777), 1000),
            live_session("newest-one", Some(7777), 9_000_000),
        ],
    );
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let queued = actions(&mut state);
    let Some(Action::SendToSession(spec)) = queued
        .into_iter()
        .find(|a| matches!(a, Action::SendToSession(_)))
    else {
        panic!("no send");
    };
    assert_eq!(spec.session_id, "newest-one");
    let flash = state.flash.clone().unwrap_or_default();
    assert!(flash.contains("sending to the most recent"), "{flash}");
}

#[test]
fn no_general_agent_driving_api_exists() {
    // The Node suite pinned "the rest of the removed prompting surface stays
    // gone" — `sendKey`, which drove y/n permission answering, was removed on
    // purpose and did not come back with `sendText`. Here the driver trait is
    // the whole surface, so the guard is that it has exactly the five methods
    // and `send_text` is documented as the revision path's only caller.
    let source = include_str!("../../../term/types.rs");
    assert!(source.contains("fn send_text("), "send_text went missing");
    assert!(
        !source.contains("fn send_key("),
        "the driver regained a general key-sending API"
    );
    assert!(
        source.contains("It is not a general \"drive the agent from the"),
        "the note explaining why send_text is narrow was removed"
    );
}

// --- resuming a conversation -------------------------------------------------

#[test]
fn resuming_says_so_when_nothing_is_archived_and_spawns_nothing() {
    // A task never worked has nothing to resume; the worker states that rather
    // than opening an empty session in some arbitrary directory. Nothing is
    // launched from the key handler either way.
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&task(991001, "A task"), conversation()),
    );
    let queued = actions(&mut state);
    assert!(
        !queued.iter().any(|a| matches!(a, Action::Launch(_))),
        "{queued:?}"
    );
    assert!(queued.iter().any(|a| matches!(a, Action::Resume(_))));
}

#[test]
fn a_still_running_session_is_focused_rather_than_resumed_twice() {
    // Resuming a live session id would start a second process against the same
    // conversation. Its terminal is already open — go there instead.
    let (_dir, mut state) = board_state();
    with_sessions(
        &mut state,
        vec![live_session("live-one", Some(991001), 1000)],
    );
    start(
        &mut state,
        start_request(&task(991001, "A task"), conversation()),
    );
    let flash = state.flash.clone().unwrap_or_default();
    assert!(
        flash.contains("still running — opening its terminal"),
        "{flash}"
    );
    let queued = actions(&mut state);
    assert!(queued.iter().any(|a| matches!(a, Action::FocusTerminal(_))));
    assert!(!queued.iter().any(|a| matches!(a, Action::Resume(_))));
    assert!(!queued.iter().any(|a| matches!(a, Action::Launch(_))));
}

#[test]
fn resuming_a_conversation_sends_no_prompt_and_moves_no_stage() {
    // Asserted at the action layer, where the decision lives: the request that
    // reaches the worker carries neither a pipeline nor a stage move, so no
    // later refactor of the worker can reintroduce either.
    let (_dir, mut state) = board_state();
    start(
        &mut state,
        start_request(&task(991001, "A task"), conversation()),
    );
    let queued = actions(&mut state);
    let Some(Action::Resume(request)) = queued.into_iter().find(|a| matches!(a, Action::Resume(_)))
    else {
        panic!("no resume");
    };
    assert!(
        !request.revision,
        "the conversation path asked for a revision"
    );
    assert!(
        request.stage_move.is_none(),
        "resuming a conversation moved the task stage"
    );

    // …and the spec it builds carries no prompt at all.
    let spec = request
        .spec("archived-session-id", "/tmp", None)
        .expect("a spec");
    assert!(
        spec.prompt.is_none(),
        "the resume path built a prompt: {:?}",
        spec.prompt
    );
    assert!(spec.flags.contains("--resume archived-session-id"));
    assert!(spec.say.contains("no prompt sent"), "{}", spec.say);
}

#[test]
fn a_revision_resumed_into_a_tab_does_carry_both() {
    // The contrast that makes the previous test meaningful.
    let (_dir, mut state) = board_state();
    start(&mut state, start_request(&task(7777, "A task"), revision()));
    let queued = actions(&mut state);
    let Some(Action::Resume(request)) = queued.into_iter().find(|a| matches!(a, Action::Resume(_)))
    else {
        panic!("no resume");
    };
    assert!(request.revision);
    assert!(request.stage_move.is_some());
    let spec = request.spec("archived", "/tmp", None).expect("a spec");
    assert!(spec.prompt.is_some());
}
