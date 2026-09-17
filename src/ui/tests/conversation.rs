//! Conversation line building and the code highlighter.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::config::ChatConfig;
use crate::types::{MessageRole, SessionStatus};
use crate::ui::conversation::{build_conversation_lines, is_tool_line, ConversationMeta};
use crate::ui::syntax::highlight_line;
use crate::ui::tests::{message, usage};

fn flat(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn compact() -> ChatConfig {
    ChatConfig {
        compact_mode: true,
        show_session_header: false,
        ..ChatConfig::default()
    }
}

#[test]
fn an_empty_conversation_says_so() {
    let lines = build_conversation_lines(&[], None, &ChatConfig::default());
    assert_eq!(flat(&lines), vec!["No conversation data"]);
}

#[test]
fn a_user_turn_carries_its_label_and_body() {
    let messages = vec![message(MessageRole::User, "fix the pairing bug")];
    let lines = build_conversation_lines(&messages, None, &compact());
    assert_eq!(flat(&lines), vec!["You:", "fix the pairing bug"]);
    assert!(lines[0].spans[0]
        .style
        .add_modifier
        .contains(Modifier::BOLD));
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Cyan));
}

#[test]
fn an_assistant_turn_annotates_its_token_count() {
    let mut msg = message(MessageRole::Assistant, "done");
    msg.usage = Some(usage(12_340));
    let lines = build_conversation_lines(&[msg], None, &compact());
    assert_eq!(flat(&lines), vec!["Claude: (12.3K)", "done"]);
}

#[test]
fn a_blank_line_separates_turns_unless_compact_mode_is_on() {
    let messages = vec![
        message(MessageRole::User, "a"),
        message(MessageRole::Assistant, "b"),
    ];
    let roomy = ChatConfig {
        show_session_header: false,
        ..ChatConfig::default()
    };
    assert_eq!(
        flat(&build_conversation_lines(&messages, None, &roomy)),
        vec!["You:", "a", "", "Claude:", "b", ""]
    );
    assert_eq!(
        flat(&build_conversation_lines(&messages, None, &compact())),
        vec!["You:", "a", "Claude:", "b"]
    );
}

#[test]
fn timestamps_appear_only_when_asked_for() {
    let cfg = ChatConfig {
        show_timestamps: true,
        ..compact()
    };
    let lines = flat(&build_conversation_lines(
        &[message(MessageRole::User, "hi")],
        None,
        &cfg,
    ));
    // Rendered in the local zone, so assert the shape rather than the hour.
    assert!(lines[0].contains(':') && (lines[0].contains("AM") || lines[0].contains("PM")));
}

#[test]
fn the_message_filter_hides_the_other_role() {
    let messages = vec![
        message(MessageRole::User, "a"),
        message(MessageRole::Assistant, "b"),
    ];
    let cfg = ChatConfig {
        message_filter: "user".into(),
        ..compact()
    };
    assert_eq!(
        flat(&build_conversation_lines(&messages, None, &cfg)),
        vec!["You:", "a"]
    );
}

#[test]
fn a_search_with_no_hits_explains_itself() {
    let cfg = ChatConfig {
        message_filter: "assistant".into(),
        search_keyword: "nothing".into(),
        ..compact()
    };
    let lines = flat(&build_conversation_lines(
        &[message(MessageRole::User, "a")],
        None,
        &cfg,
    ));
    assert_eq!(
        lines,
        vec!["No messages match (filter: assistant) (search: \"nothing\")"]
    );
}

#[test]
fn max_lines_per_message_caps_the_body_and_says_how_much_it_hid() {
    let cfg = ChatConfig {
        max_lines_per_message: 2,
        ..compact()
    };
    let lines = flat(&build_conversation_lines(
        &[message(MessageRole::User, "one\ntwo\nthree\nfour")],
        None,
        &cfg,
    ));
    assert_eq!(lines, vec!["You:", "one", "two", "... (2 more lines)"]);
}

#[test]
fn one_hidden_line_is_singular() {
    let cfg = ChatConfig {
        max_lines_per_message: 1,
        ..compact()
    };
    let lines = flat(&build_conversation_lines(
        &[message(MessageRole::User, "one\ntwo")],
        None,
        &cfg,
    ));
    assert_eq!(lines.last().unwrap(), "... (1 more line)");
}

#[test]
fn tool_lines_are_recognised_including_unknown_mcp_tools() {
    assert!(is_tool_line("[Read: src/ui.rs]"));
    assert!(is_tool_line("[Bash: cargo test]"));
    assert!(is_tool_line("[Using tool: something]"));
    assert!(is_tool_line("[mcp_odoo_execute: x]"));
    assert!(!is_tool_line("not a tool line"));
    assert!(!is_tool_line("[not-a-tool]"));
}

#[test]
fn a_tool_only_message_can_be_hidden_or_collapsed() {
    let mut msg = message(MessageRole::Assistant, "[Read: a.rs]\n[Read: b.rs]");
    msg.has_tool_use = true;
    let hide = ChatConfig {
        tool_display: "hide".into(),
        ..compact()
    };
    let collapse = ChatConfig {
        tool_display: "collapse".into(),
        ..compact()
    };
    assert_eq!(
        flat(&build_conversation_lines(
            std::slice::from_ref(&msg),
            None,
            &hide
        )),
        Vec::<String>::new()
    );
    assert_eq!(
        flat(&build_conversation_lines(&[msg], None, &collapse)),
        vec!["[2 tool call(s)]"]
    );
}

#[test]
fn prose_is_never_hidden_by_the_tool_setting() {
    // Only a message that is *nothing but* tool calls may be collapsed.
    let mut msg = message(MessageRole::Assistant, "[Read: a.rs]\nand here is why");
    msg.has_tool_use = true;
    let cfg = ChatConfig {
        tool_display: "hide".into(),
        ..compact()
    };
    let lines = flat(&build_conversation_lines(&[msg], None, &cfg));
    assert!(lines.iter().any(|l| l == "and here is why"), "{lines:?}");
}

#[test]
fn a_tool_heading_takes_the_theme_tool_colour() {
    let msg = message(MessageRole::Assistant, "[Read: a.rs]\nprose");
    let lines = build_conversation_lines(&[msg], None, &compact());
    let heading = &lines[1];
    assert_eq!(heading.spans[0].style.fg, Some(Color::Yellow));
}

#[test]
fn an_edit_diff_body_gets_added_and_removed_backgrounds() {
    let msg = message(
        MessageRole::Assistant,
        "[Edit: a.rs]\n- let x = 1;\n+ let x = 2;",
    );
    let lines = build_conversation_lines(&[msg], None, &compact());
    let removed = &lines[2];
    let added = &lines[3];
    assert_eq!(removed.spans[0].content, "-");
    assert_eq!(removed.spans[0].style.fg, Some(Color::Red));
    assert_eq!(
        removed.spans[0].style.bg,
        Some(Color::Rgb(0x5a, 0x1f, 0x1f))
    );
    assert_eq!(added.spans[0].content, "+");
    assert_eq!(added.spans[0].style.bg, Some(Color::Rgb(0x1e, 0x4d, 0x24)));
    // The body is highlighted *inside* the diff background, not on top of it.
    assert!(added
        .spans
        .iter()
        .any(|s| s.content.as_ref() == "let" && s.style.fg == Some(Color::Magenta)));
}

#[test]
fn a_fenced_block_is_highlighted_and_its_fences_are_dim() {
    let msg = message(
        MessageRole::Assistant,
        "```rust\nlet n = 42; // note\n```\nafter",
    );
    let lines = build_conversation_lines(&[msg], None, &compact());
    assert_eq!(lines[1].spans[0].style.fg, Some(Color::DarkGray));
    let code: Vec<(&str, Option<Color>)> = lines[2]
        .spans
        .iter()
        .map(|s| (s.content.as_ref(), s.style.fg))
        .collect();
    assert!(code.contains(&("let", Some(Color::Magenta))), "{code:?}");
    assert!(code.contains(&("42", Some(Color::Yellow))), "{code:?}");
    assert!(
        code.contains(&("// note", Some(Color::DarkGray))),
        "{code:?}"
    );
    // Text after the closing fence is plain again.
    assert_eq!(lines[4].spans[0].style.fg, None);
}

#[test]
fn the_session_header_shows_context_and_last_activity() {
    let meta = ConversationMeta {
        last_usage: Some(usage(50_000)),
        last_timestamp: "2026-09-16T14:05:06.000Z".into(),
        session_id: "abcd".into(),
        status: None,
        activity_detail: String::new(),
    };
    let lines = flat(&build_conversation_lines(
        &[message(MessageRole::User, "hi")],
        Some(&meta),
        &ChatConfig::default(),
    ));
    assert!(lines[0].contains("50K tokens (25%)"), "{lines:?}");
    assert!(lines[0].contains("last:"), "{lines:?}");
}

#[test]
fn the_status_footer_says_how_to_answer_a_permission_prompt() {
    let of = |status: SessionStatus, detail: &str| {
        let meta = ConversationMeta {
            status: Some(status),
            activity_detail: detail.into(),
            ..ConversationMeta::default()
        };
        flat(&build_conversation_lines(
            &[message(MessageRole::User, "hi")],
            Some(&meta),
            &compact(),
        ))
        .pop()
        .unwrap()
    };
    assert_eq!(of(SessionStatus::Working, "reading"), "● reading...");
    assert_eq!(of(SessionStatus::Idle, ""), "● idle");
    assert!(of(SessionStatus::Awaiting, "").contains("open its terminal to answer"));
    assert_eq!(of(SessionStatus::AwaitingInput, ""), "● awaiting input");
}

// --- highlighter ------------------------------------------------------------

fn tokens(line: &str) -> Vec<(String, Option<Color>)> {
    highlight_line(line, Style::default())
        .into_iter()
        .map(|s| (s.content.to_string(), s.style.fg))
        .collect()
}

#[test]
fn strings_numbers_and_comments_each_get_their_colour() {
    let got = tokens("x = \"hi\" + 0xff # why");
    assert!(
        got.contains(&("\"hi\"".into(), Some(Color::Green))),
        "{got:?}"
    );
    assert!(
        got.contains(&("0xff".into(), Some(Color::Yellow))),
        "{got:?}"
    );
    assert!(
        got.contains(&("# why".into(), Some(Color::DarkGray))),
        "{got:?}"
    );
}

#[test]
fn keywords_and_builtins_are_told_apart() {
    let got = tokens("pub fn main() { let ok = true; }");
    assert!(
        got.contains(&("pub".into(), Some(Color::Magenta))),
        "{got:?}"
    );
    assert!(
        got.contains(&("true".into(), Some(Color::Yellow))),
        "{got:?}"
    );
    // Ordinary text stays plain, and the untokenised runs between coloured
    // tokens are merged — one span per run, never one per character.
    let plain: String = got
        .iter()
        .filter(|(_, colour)| colour.is_none())
        .map(|(token, _)| token.as_str())
        .collect();
    assert!(plain.contains("main"), "{got:?}");
    assert!(
        got.len() <= 10,
        "identifier was split per character: {got:?}"
    );
}

#[test]
fn a_decorator_is_highlighted() {
    let got = tokens("@property");
    assert_eq!(got, vec![("@property".to_string(), Some(Color::Cyan))]);
}

#[test]
fn an_unterminated_string_does_not_swallow_the_line() {
    let got = tokens("\"oops");
    let rendered: String = got.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(rendered, "\"oops");
    assert!(got.iter().all(|(_, colour)| colour.is_none()));
}

#[test]
fn highlighting_never_loses_or_reorders_characters() {
    for line in [
        "",
        "   ",
        "let x=1;//end",
        "def f(a, b): return a+b  # add",
        "日本語 = \"値\"",
        "1.5 + 1. + 0x1F",
    ] {
        let rendered: String = highlight_line(line, Style::default())
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(rendered, line, "round trip failed for {line:?}");
    }
}

#[test]
fn a_base_style_carries_through_the_highlighting() {
    let base = Style::default().bg(Color::Rgb(1, 2, 3));
    let spans = highlight_line("let x = 1;", base);
    assert!(spans
        .iter()
        .all(|s| s.style.bg == Some(Color::Rgb(1, 2, 3))));
}

#[test]
fn a_session_that_arrived_over_the_wire_still_opens_its_transcript() {
    // The conversation is never streamed: the daemon ships `sessionFile` and
    // the dashboard reads the file itself, on the machine both are running on
    // (Node's `index.js` watched it the same way). This drives the whole path —
    // wire round trip, selection, cursor — because a stripped `sessionFile`
    // would show as "No conversation data" with a session plainly selected.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let transcript = crate::transcript::fixtures::transcripts()
        .into_iter()
        .next()
        .expect("a fixture transcript");

    let mut live = crate::ui::tests::session(
        "abcd1234",
        "/Users/x/dev/alpha",
        crate::types::SessionStatus::Working,
    );
    live.session_file = Some(transcript.clone());
    live.activity_detail = "reading".to_string();

    // Server → JSON → client, exactly as `RemoteFeed` receives it.
    let json = serde_json::to_string(&crate::daemon::wire_session(&live)).unwrap();
    let received: crate::types::Session = serde_json::from_str(&json).unwrap();
    assert_eq!(received.session_file.as_ref(), Some(&transcript));

    let (_dir, mut state) = crate::ui::tests::sessions_state();
    state.apply_sessions(crate::ui::feed::group_sessions(vec![received]));
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    crate::ui::keys::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        area,
    );
    crate::ui::keys::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        area,
    );

    let file = match state.take_actions().into_iter().next() {
        Some(crate::ui::state::Action::SelectSession { session_file, .. }) => session_file,
        other => panic!("expected a select action, got {other:?}"),
    };
    assert_eq!(file.as_ref(), Some(&transcript), "the path did not survive");

    let cursor = crate::ui::run::open_conversation(&mut state, file);
    assert!(cursor.is_some(), "the cursor did not attach");
    assert!(
        !state.conv.messages.is_empty(),
        "the pane would say 'No conversation data'"
    );
    let lines = build_conversation_lines(
        &state.conv.messages,
        state.conv.meta.as_ref(),
        &ChatConfig::default(),
    );
    assert_ne!(flat(&lines), vec!["No conversation data"]);
}
