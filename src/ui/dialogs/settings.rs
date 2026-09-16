//! The chat settings grid.
//!
//! Ported from `SETTINGS` + `SettingsMenu` in the Node app's
//! `src/tui/dialogs.js`: the same rows in the same order, `←→` cycling enum and
//! number fields, `Enter` editing the two text ones. Node stored chat config as
//! a loose object and indexed it by string key; here it is a typed struct, so
//! the grid is a list of [`Field`]s that know how to read and write one field
//! each. Every change saves immediately, as it did in Node — the grid has no
//! cancel, only undo by cycling back.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::config::ChatConfig;
use crate::ui::dialogs::widgets::{hint, render_modal};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::theme::{color_from_name, named_theme, COLOR_NAMES, THEME_NAMES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Theme,
    UserColor,
    AssistantColor,
    ToolColor,
    CodeColor,
    UserLabel,
    AssistantLabel,
    ShowTimestamps,
    ToolDisplay,
    CompactMode,
    MaxLines,
    MessageFilter,
    ConversationWidth,
    SwapPanels,
    ShowSessionHeader,
}

#[derive(Debug, Clone, Copy)]
pub enum Row {
    Separator(&'static str),
    Field(Field),
}

/// The grid, in the order Node listed it.
pub const ROWS: [Row; 18] = [
    Row::Field(Field::Theme),
    Row::Field(Field::UserColor),
    Row::Field(Field::AssistantColor),
    Row::Field(Field::ToolColor),
    Row::Field(Field::CodeColor),
    Row::Field(Field::UserLabel),
    Row::Field(Field::AssistantLabel),
    Row::Separator("──── Display ────"),
    Row::Field(Field::ShowTimestamps),
    Row::Field(Field::ToolDisplay),
    Row::Field(Field::CompactMode),
    Row::Field(Field::MaxLines),
    Row::Separator("──── Filter ────"),
    Row::Field(Field::MessageFilter),
    Row::Separator("──── Layout ────"),
    Row::Field(Field::ConversationWidth),
    Row::Field(Field::SwapPanels),
    Row::Field(Field::ShowSessionHeader),
];

const TOOL_DISPLAY: [&str; 3] = ["show", "hide", "collapse"];
const MESSAGE_FILTER: [&str; 3] = ["all", "user", "assistant"];

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Theme => "Theme",
            Field::UserColor => "User color",
            Field::AssistantColor => "Assistant color",
            Field::ToolColor => "Tool color",
            Field::CodeColor => "Code color",
            Field::UserLabel => "User label",
            Field::AssistantLabel => "Assistant label",
            Field::ShowTimestamps => "Timestamps",
            Field::ToolDisplay => "Tool calls",
            Field::CompactMode => "Compact mode",
            Field::MaxLines => "Max lines/msg",
            Field::MessageFilter => "Show messages",
            Field::ConversationWidth => "Panel width",
            Field::SwapPanels => "Swap panels",
            Field::ShowSessionHeader => "Session header",
        }
    }

    /// The two free-text fields. Everything else cycles.
    pub fn is_text(self) -> bool {
        matches!(self, Field::UserLabel | Field::AssistantLabel)
    }

    /// Colour fields get a swatch of the value beside them.
    pub fn is_color(self) -> bool {
        matches!(
            self,
            Field::UserColor | Field::AssistantColor | Field::ToolColor | Field::CodeColor
        )
    }

    pub fn display(self, chat: &ChatConfig) -> String {
        match self {
            Field::Theme => chat.theme.clone(),
            Field::UserColor => chat.user_color.clone(),
            Field::AssistantColor => chat.assistant_color.clone(),
            Field::ToolColor => chat.tool_color.clone(),
            Field::CodeColor => chat.code_color.clone(),
            Field::UserLabel => chat.user_label.clone(),
            Field::AssistantLabel => chat.assistant_label.clone(),
            Field::ShowTimestamps => on_off(chat.show_timestamps),
            Field::ToolDisplay => chat.tool_display.clone(),
            Field::CompactMode => on_off(chat.compact_mode),
            Field::MaxLines => {
                if chat.max_lines_per_message == 0 {
                    "unlimited".to_string()
                } else {
                    chat.max_lines_per_message.to_string()
                }
            }
            Field::MessageFilter => chat.message_filter.clone(),
            Field::ConversationWidth => format!("{}%", chat.conversation_width),
            Field::SwapPanels => on_off(chat.swap_panels),
            Field::ShowSessionHeader => on_off(chat.show_session_header),
        }
    }

    pub fn set_text(self, chat: &mut ChatConfig, value: String) {
        match self {
            Field::UserLabel => chat.user_label = value,
            Field::AssistantLabel => chat.assistant_label = value,
            _ => {}
        }
    }

    pub fn raw_text(self, chat: &ChatConfig) -> String {
        match self {
            Field::UserLabel => chat.user_label.clone(),
            Field::AssistantLabel => chat.assistant_label.clone(),
            _ => String::new(),
        }
    }

    pub fn cycle(self, chat: &mut ChatConfig, dir: i32) {
        match self {
            Field::Theme => {
                chat.theme = step_str(&THEME_NAMES, &chat.theme, dir);
                // Cycling the theme writes its six values into the config. That
                // is what makes `resolve_theme`'s "differs from the base" rule
                // mean "the user chose this" rather than "this is a leftover".
                let theme = named_theme(&chat.theme);
                chat.user_color = theme.user_color;
                chat.assistant_color = theme.assistant_color;
                chat.tool_color = theme.tool_color;
                chat.code_color = theme.code_color;
                chat.user_label = theme.user_label;
                chat.assistant_label = theme.assistant_label;
            }
            Field::UserColor => chat.user_color = step_str(&COLOR_NAMES, &chat.user_color, dir),
            Field::AssistantColor => {
                chat.assistant_color = step_str(&COLOR_NAMES, &chat.assistant_color, dir)
            }
            Field::ToolColor => chat.tool_color = step_str(&COLOR_NAMES, &chat.tool_color, dir),
            Field::CodeColor => chat.code_color = step_str(&COLOR_NAMES, &chat.code_color, dir),
            Field::ShowTimestamps => chat.show_timestamps = !chat.show_timestamps,
            Field::CompactMode => chat.compact_mode = !chat.compact_mode,
            Field::SwapPanels => chat.swap_panels = !chat.swap_panels,
            Field::ShowSessionHeader => chat.show_session_header = !chat.show_session_header,
            Field::ToolDisplay => {
                chat.tool_display = step_str(&TOOL_DISPLAY, &chat.tool_display, dir)
            }
            Field::MessageFilter => {
                chat.message_filter = step_str(&MESSAGE_FILTER, &chat.message_filter, dir)
            }
            Field::MaxLines => {
                chat.max_lines_per_message = (chat.max_lines_per_message + dir as i64 * 5).max(0);
            }
            Field::ConversationWidth => {
                let next = chat.conversation_width as i32 + dir * 5;
                chat.conversation_width = next.clamp(10, 80) as u16;
            }
            Field::UserLabel | Field::AssistantLabel => {}
        }
    }
}

fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_string()
}

/// Step through a fixed option list, wrapping. A value that is not in the list
/// (a hand-edited config) lands on the first option rather than being rejected.
fn step_str(options: &[&str], current: &str, dir: i32) -> String {
    let len = options.len() as i32;
    let at = options.iter().position(|o| *o == current).unwrap_or(0) as i32;
    options[(((at + dir) % len + len) % len) as usize].to_string()
}

/// The editable rows, in order — what `↑↓` walks. Separators are skipped.
pub fn editable() -> Vec<Field> {
    ROWS.iter()
        .filter_map(|row| match row {
            Row::Field(field) => Some(*field),
            Row::Separator(_) => None,
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct SettingsDialog {
    pub row: usize,
    pub editing: Option<String>,
}

impl SettingsDialog {
    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let fields = editable();
        let field = fields[self.row.min(fields.len() - 1)];

        if let Some(buffer) = self.editing.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let value = buffer.clone();
                    let mut chat = ctx.config.chat().clone();
                    field.set_text(&mut chat, value);
                    let _ = ctx.config.save_chat_config(chat);
                    self.editing = None;
                }
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace | KeyCode::Delete => {
                    buffer.pop();
                }
                KeyCode::Char(ch) if !key.modifiers.intersects(CTRL_ALT) => buffer.push(ch),
                _ => {}
            }
            return DialogOutcome::Stay;
        }

        let mut cycle = |dir: i32| {
            let mut chat = ctx.config.chat().clone();
            field.cycle(&mut chat, dir);
            let _ = ctx.config.save_chat_config(chat);
        };

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.row = (self.row + 1).min(fields.len() - 1);
            }
            KeyCode::Right | KeyCode::Char('l') => cycle(1),
            KeyCode::Left | KeyCode::Char('h') => cycle(-1),
            KeyCode::Enter => {
                if field.is_text() {
                    self.editing = Some(field.raw_text(ctx.config.chat()));
                } else {
                    cycle(1);
                }
            }
            KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('q') => return DialogOutcome::Close,
            _ => {}
        }
        DialogOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, chat: &ChatConfig) {
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(ROWS.len() + 2);
        let mut editable_index = 0usize;
        for row in ROWS {
            match row {
                Row::Separator(label) => lines.push(hint(label)),
                Row::Field(field) => {
                    let selected = editable_index == self.row;
                    let value = match (&self.editing, selected && field.is_text()) {
                        (Some(buffer), true) => format!("{buffer}█"),
                        _ => field.display(chat),
                    };
                    let arrows = if field.is_text() { "" } else { " ◄►" };
                    let mut spans = vec![Span::styled(
                        if selected { "› " } else { "  " }.to_string(),
                        Style::default().fg(color_from_name("cyan")),
                    )];
                    let body_style = if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(format!("{:<16}", field.label()), body_style));
                    if field.is_color() {
                        spans.push(Span::styled(
                            "● ",
                            Style::default().fg(color_from_name(&field.display(chat))),
                        ));
                    }
                    spans.push(Span::styled(format!("[{value}]{arrows}"), body_style));
                    lines.push(Line::from(spans));
                    editable_index += 1;
                }
            }
        }
        lines.push(Line::default());
        lines.push(hint(if self.editing.is_some() {
            "Type to edit  Enter save  Esc cancel"
        } else {
            "↑↓ move  ←→ change  Enter edit text  Esc close"
        }));
        render_modal(
            frame,
            area,
            " Chat Settings ",
            color_from_name("cyan"),
            lines,
            48.min(area.width),
        );
    }
}

const CTRL_ALT: crossterm::event::KeyModifiers = crossterm::event::KeyModifiers::from_bits_truncate(
    crossterm::event::KeyModifiers::CONTROL.bits() | crossterm::event::KeyModifiers::ALT.bits(),
);
