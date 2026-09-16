//! The conversation pane: parsed turns in, styled lines out.
//!
//! Ported from the Node app's `src/tui/conversation.js`. Node produced tagged
//! strings that `markup.js` re-parsed into styled runs on every frame — three
//! string passes per row. Brief §10 mandate #7 deletes that round-trip, so this
//! builds [`Line`]s straight away and the pane only ever formats what fits.

use chrono::Local;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::ChatConfig;
use crate::types::{ConversationMessage, MessageRole, SessionStatus, Usage};
use crate::ui::syntax::highlight_line;
use crate::ui::theme::{color_from_name, resolve_theme, Theme};
use crate::util::format_context_usage;

/// Enough of the selected session for the header and status footer. Kept apart
/// from [`crate::types::Session`] so the pane can be built from a cursor's view
/// of a transcript, which is all the dashboard has between scans.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationMeta {
    pub last_usage: Option<Usage>,
    pub last_timestamp: String,
    pub session_id: String,
    pub status: Option<SessionStatus>,
    pub activity_detail: String,
}

const REMOVED_BG: &str = "#5a1f1f";
const ADDED_BG: &str = "#1e4d24";

fn gray() -> Style {
    Style::default().fg(color_from_name("gray"))
}

fn gray_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), gray()))
}

/// Build the whole pane. `cfg` is the live chat config; the theme is resolved
/// once here rather than per message.
pub fn build_conversation_lines(
    messages: &[ConversationMessage],
    meta: Option<&ConversationMeta>,
    cfg: &ChatConfig,
) -> Vec<Line<'static>> {
    if messages.is_empty() {
        return vec![gray_line("No conversation data")];
    }
    let theme = resolve_theme(cfg);
    let mut lines: Vec<Line<'static>> = Vec::new();

    if cfg.show_session_header {
        if let Some(meta) = meta {
            let mut parts: Vec<String> = Vec::new();
            if meta.last_usage.is_some() {
                parts.push(format_context_usage(meta.last_usage.as_ref()));
            }
            if let Some(stamp) = clock(&meta.last_timestamp, "%I:%M %p") {
                parts.push(format!("last: {stamp}"));
            }
            if !parts.is_empty() {
                lines.push(gray_line(parts.join("  │  ")));
                lines.push(Line::default());
            }
        }
    }

    let filtered = filter_messages(messages, cfg);
    if filtered.is_empty() {
        let f = if cfg.message_filter != "all" {
            format!(" (filter: {})", cfg.message_filter)
        } else {
            String::new()
        };
        let s = if cfg.search_keyword.is_empty() {
            String::new()
        } else {
            format!(" (search: \"{}\")", cfg.search_keyword)
        };
        return vec![gray_line(format!("No messages match{f}{s}"))];
    }

    let mut collapsed_tools = 0usize;
    for msg in filtered {
        match msg.role {
            MessageRole::User => {
                flush_tools(&mut lines, &mut collapsed_tools, cfg, &theme);
                push_timestamp(&mut lines, &msg.timestamp, cfg);
                lines.push(label_line(&theme.user_label, theme.user(), None));
                let body: Vec<Line<'static>> = msg
                    .text
                    .split('\n')
                    .map(|l| Line::raw(l.to_string()))
                    .collect();
                push_capped(&mut lines, body, cfg.max_lines_per_message);
                if !cfg.compact_mode {
                    lines.push(Line::default());
                }
            }
            MessageRole::Assistant => {
                let raw: Vec<&str> = msg.text.split('\n').collect();
                if is_only_tools(&raw) && msg.has_tool_use {
                    if cfg.tool_display == "hide" {
                        continue;
                    }
                    if cfg.tool_display == "collapse" {
                        collapsed_tools += raw.iter().filter(|l| is_tool_line(l)).count();
                        continue;
                    }
                }
                flush_tools(&mut lines, &mut collapsed_tools, cfg, &theme);
                push_timestamp(&mut lines, &msg.timestamp, cfg);
                lines.push(label_line(
                    &theme.assistant_label,
                    theme.assistant(),
                    token_annotation(msg.usage.as_ref()),
                ));
                let body = render_assistant_body(&raw, &theme);
                push_capped(&mut lines, body, cfg.max_lines_per_message);
                if !cfg.compact_mode {
                    lines.push(Line::default());
                }
            }
        }
    }
    flush_tools(&mut lines, &mut collapsed_tools, cfg, &theme);

    if let Some(meta) = meta {
        if let Some(status) = status_line(meta) {
            lines.push(status);
        }
    }
    lines
}

fn filter_messages<'a>(
    messages: &'a [ConversationMessage],
    cfg: &ChatConfig,
) -> Vec<&'a ConversationMessage> {
    let keyword = cfg.search_keyword.to_lowercase();
    messages
        .iter()
        .filter(|m| match cfg.message_filter.as_str() {
            "user" => m.role == MessageRole::User,
            "assistant" => m.role == MessageRole::Assistant,
            _ => true,
        })
        .filter(|m| keyword.is_empty() || m.text.to_lowercase().contains(&keyword))
        .collect()
}

/// `[Read: …]`, `[Bash: …]`, `[Using tool: …]` — the shapes
/// [`crate::transcript::format_tool_use`] emits. The trailing `\w+:` arm is why
/// an MCP tool nobody has heard of still renders in the tool colour.
pub fn is_tool_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else {
        return false;
    };
    if rest.starts_with("Using tool:") {
        return true;
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    !word.is_empty() && rest[word.len()..].starts_with(':')
}

/// A message that is nothing but tool calls (and their diff body) — the only
/// kind `toolDisplay` is allowed to hide or collapse, so prose never vanishes.
fn is_only_tools(raw: &[&str]) -> bool {
    let has_header = raw.iter().any(|l| is_tool_line(l));
    has_header
        && raw.iter().all(|l| {
            is_tool_line(l) || l.starts_with("- ") || l.starts_with("+ ") || l.trim().is_empty()
        })
}

fn flush_tools(lines: &mut Vec<Line<'static>>, count: &mut usize, cfg: &ChatConfig, theme: &Theme) {
    if *count > 0 && cfg.tool_display == "collapse" {
        lines.push(Line::from(Span::styled(
            format!("[{count} tool call(s)]"),
            Style::default().fg(theme.tool()),
        )));
        *count = 0;
    }
}

fn label_line(
    label: &str,
    colour: ratatui::style::Color,
    annotation: Option<String>,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label}:"),
        Style::default().fg(colour).add_modifier(Modifier::BOLD),
    )];
    if let Some(annotation) = annotation {
        spans.push(Span::styled(annotation, gray()));
    }
    Line::from(spans)
}

fn token_annotation(usage: Option<&Usage>) -> Option<String> {
    let total = usage?.total_tokens();
    if total == 0 {
        return None;
    }
    Some(format!(" ({:.1}K)", total as f64 / 1000.0))
}

fn push_timestamp(lines: &mut Vec<Line<'static>>, timestamp: &str, cfg: &ChatConfig) {
    if !cfg.show_timestamps {
        return;
    }
    if let Some(stamp) = clock(timestamp, "%I:%M:%S %p") {
        lines.push(gray_line(stamp));
    }
}

/// Cap one message's body, with a "… (N more lines)" tail. `0` is unlimited.
fn push_capped(lines: &mut Vec<Line<'static>>, body: Vec<Line<'static>>, max: i64) {
    if max <= 0 || body.len() <= max as usize {
        lines.extend(body);
        return;
    }
    let max = max as usize;
    let remaining = body.len() - max;
    lines.extend(body.into_iter().take(max));
    let plural = if remaining > 1 { "s" } else { "" };
    lines.push(gray_line(format!("... ({remaining} more line{plural})")));
}

/// Fenced code, `Edit` diff bodies and tool headings, in one pass over the
/// message. The two diff backgrounds are hex rather than palette colours
/// deliberately: red-on-red has to stay legible whatever the terminal theme is.
fn render_assistant_body(raw: &[&str], theme: &Theme) -> Vec<Line<'static>> {
    let removed = Style::default().bg(color_from_name(REMOVED_BG));
    let added = Style::default().bg(color_from_name(ADDED_BG));
    let mut out = Vec::with_capacity(raw.len());
    let mut in_code = false;
    let mut in_diff = false;

    for line in raw {
        if line.starts_with("```") {
            in_code = !in_code;
            in_diff = false;
            out.push(gray_line(line.to_string()));
        } else if in_code {
            out.push(Line::from(highlight_line(line, Style::default())));
        } else if is_tool_line(line) {
            in_diff = line.starts_with("[Edit:");
            out.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.tool()),
            )));
        } else if in_diff && line.starts_with("- ") {
            out.push(diff_line(
                line,
                '-',
                removed.fg(color_from_name("red")),
                removed,
            ));
        } else if in_diff && line.starts_with("+ ") {
            out.push(diff_line(
                line,
                '+',
                added.fg(color_from_name("green")),
                added,
            ));
        } else {
            in_diff = false;
            out.push(Line::raw(line.to_string()));
        }
    }
    out
}

fn diff_line(line: &str, marker: char, marker_style: Style, body: Style) -> Line<'static> {
    let mut spans = vec![
        Span::styled(marker.to_string(), marker_style),
        Span::styled(" ", body),
    ];
    spans.extend(highlight_line(&line[2..], body));
    Line::from(spans)
}

/// The one-line footer under the transcript. `awaiting` says how to answer,
/// because the answer has to be typed in the session's own terminal.
fn status_line(meta: &ConversationMeta) -> Option<Line<'static>> {
    let status = meta.status?;
    let detail = if meta.activity_detail.is_empty() {
        "working"
    } else {
        &meta.activity_detail
    };
    Some(match status {
        SessionStatus::Working | SessionStatus::Compacting | SessionStatus::Starting => {
            let colour = if status == SessionStatus::Compacting {
                "magenta"
            } else {
                "green"
            };
            Line::from(Span::styled(
                format!("● {detail}..."),
                Style::default().fg(color_from_name(colour)),
            ))
        }
        SessionStatus::Idle => gray_line("● idle"),
        SessionStatus::Awaiting => Line::from(vec![
            Span::styled(
                "● awaiting permission",
                Style::default().fg(color_from_name("yellow")),
            ),
            Span::styled("  o", gray()),
            Span::styled(" open its terminal to answer", gray()),
        ]),
        SessionStatus::AwaitingInput => Line::from(Span::styled(
            "● awaiting input",
            Style::default().fg(color_from_name("cyan")),
        )),
    })
}

/// An ISO timestamp as a local wall clock, or `None` when it will not parse —
/// a garbled stamp should leave the row alone rather than print `Invalid Date`.
fn clock(timestamp: &str, format: &str) -> Option<String> {
    let parsed = crate::util::parse_timestamp(timestamp)?;
    Some(parsed.with_timezone(&Local).format(format).to_string())
}
