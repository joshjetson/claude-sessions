//! The four dialog primitives, plus the modal frame they all sit in.
//!
//! Ported from the top of the Node app's `src/tui/dialogs.js` — the file the
//! brief holds up as the cautionary tale at 1457 lines. Everything here is
//! shared machinery; the dialogs themselves live one module per area.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::ui::theme::color_from_name;

/// A centred modal of `width` columns and as many rows as its content needs,
/// clamped to what the body area can hold.
pub fn modal_area(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Width for a dialog: `preferred`, but never wider than the body less a gutter.
pub fn dialog_width(area: Rect, preferred: u16) -> u16 {
    preferred.min(area.width.saturating_sub(6)).max(20)
}

/// Draw a bordered, centred modal over whatever is underneath it.
pub fn render_modal(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    border: Color,
    lines: Vec<Line<'static>>,
    width: u16,
) {
    let height = (lines.len() as u16).saturating_add(2).min(area.height);
    let rect = modal_area(area, width, height);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(color_from_name("cyan")),
        ));
    let inner = block.inner(rect);
    // Clear first: a modal that lets the tree show through its own body reads
    // as a rendering bug rather than as a dialog.
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);
}

pub fn gray() -> Style {
    Style::default().fg(color_from_name("gray"))
}

pub fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), gray()))
}

// --- SelectList -------------------------------------------------------------

/// A windowed, keyboard-driven list. The window is computed from the visible
/// height, so a hundred-row menu costs the same to draw as a three-row one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectList {
    pub rows: Vec<Line<'static>>,
    pub sel: usize,
}

/// What a key did to a [`SelectList`]. `Unhandled` is the extra-key hook Node
/// spelled as an `onKey` callback: the owning dialog gets first refusal on
/// anything the list does not use itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOutcome {
    Stay,
    Select(usize),
    Cancel,
    Unhandled(KeyEvent),
}

impl SelectList {
    pub fn new(rows: Vec<Line<'static>>) -> Self {
        SelectList { rows, sel: 0 }
    }

    /// How many rows fit, and which one is at the top. Ported from the Node
    /// original: at least three rows even in a short terminal, and the window
    /// only scrolls once the selection would leave it.
    pub fn window(&self, body_height: u16) -> (usize, usize) {
        let max_visible = 3.max(
            (body_height as usize)
                .saturating_sub(6)
                .min(self.rows.len()),
        );
        let top = if self.sel >= max_visible {
            self.sel - max_visible + 1
        } else {
            0
        };
        let top = top.min(self.rows.len().saturating_sub(max_visible));
        (top, max_visible)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ListOutcome {
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                ListOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(last);
                ListOutcome::Stay
            }
            KeyCode::Enter => ListOutcome::Select(self.sel),
            KeyCode::Esc => ListOutcome::Cancel,
            _ => ListOutcome::Unhandled(key),
        }
    }

    /// The visible rows, with the selected one marked. Only the window is
    /// formatted — brief §10 mandate #7.
    pub fn lines(&self, body_height: u16) -> Vec<Line<'static>> {
        let (top, max_visible) = self.window(body_height);
        self.rows
            .iter()
            .enumerate()
            .skip(top)
            .take(max_visible)
            .map(|(i, row)| {
                let mut spans = Vec::with_capacity(row.spans.len() + 1);
                if i == self.sel {
                    spans.push(Span::styled(
                        "› ",
                        Style::default().fg(color_from_name("cyan")),
                    ));
                    spans.extend(row.spans.iter().map(|s| {
                        Span::styled(s.content.to_string(), s.style.add_modifier(Modifier::BOLD))
                    }));
                } else {
                    spans.push(Span::raw("  "));
                    spans.extend(row.spans.iter().cloned());
                }
                Line::from(spans)
            })
            .collect()
    }
}

// --- InlineChoice -----------------------------------------------------------

/// A horizontal `[ Cancel ] [ Do it ]` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineChoice {
    pub options: Vec<String>,
    pub sel: usize,
}

impl InlineChoice {
    /// Selection STARTS ON CANCEL. Put the safe option first and Enter-mashing
    /// through a destructive confirmation cancels instead of firing — the
    /// property is the reason this widget exists rather than a bare `y/n`.
    pub fn new(options: impl IntoIterator<Item = impl Into<String>>) -> Self {
        InlineChoice {
            options: options.into_iter().map(Into::into).collect(),
            sel: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ListOutcome {
        let last = self.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.sel = self.sel.saturating_sub(1);
                ListOutcome::Stay
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.sel = (self.sel + 1).min(last);
                ListOutcome::Stay
            }
            KeyCode::Enter => ListOutcome::Select(self.sel),
            // Esc picks index 0, which is the safe option by construction.
            KeyCode::Esc => ListOutcome::Select(0),
            _ => ListOutcome::Unhandled(key),
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut spans = Vec::new();
        for (i, option) in self.options.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if i == self.sel {
                Style::default()
                    .bg(color_from_name("cyan"))
                    .fg(color_from_name("black"))
                    .add_modifier(Modifier::BOLD)
            } else {
                gray()
            };
            spans.push(Span::styled(format!(" {option} "), style));
        }
        vec![
            Line::from(spans),
            hint("←→ choose  ·  Enter confirm  ·  Esc cancel"),
        ]
    }
}

// --- TextPrompt -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOutcome {
    Stay,
    Submit(String),
    Cancel,
}

/// A single- or multi-line text field with a block cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextPrompt {
    pub value: String,
    pub multiline: bool,
}

impl TextPrompt {
    pub fn new(initial: impl Into<String>, multiline: bool) -> Self {
        TextPrompt {
            value: initial.into(),
            multiline,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PromptOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => PromptOutcome::Cancel,
            KeyCode::Enter => {
                // In a multiline field Enter is a newline and Ctrl-S submits,
                // so a pasted paragraph does not send itself halfway through.
                if self.multiline && !ctrl {
                    self.value.push('\n');
                    PromptOutcome::Stay
                } else {
                    PromptOutcome::Submit(self.value.clone())
                }
            }
            KeyCode::Char('s') if self.multiline && ctrl => {
                PromptOutcome::Submit(self.value.clone())
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.value.pop();
                PromptOutcome::Stay
            }
            KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.value.push(ch);
                PromptOutcome::Stay
            }
            _ => PromptOutcome::Stay,
        }
    }

    /// The field as drawn: hard-wrapped to `inner_width` with a block cursor at
    /// the end, tailed to the last `max_lines` so the cursor stays visible when
    /// the input outgrows the dialog.
    pub fn wrapped(&self, inner_width: usize, max_lines: usize) -> Vec<String> {
        let inner_width = inner_width.max(1);
        let mut wrapped: Vec<String> = Vec::new();
        let with_cursor = format!("{}█", self.value);
        for logical in with_cursor.split('\n') {
            if logical.is_empty() {
                wrapped.push(String::new());
                continue;
            }
            let mut cur = String::new();
            let mut cur_w = 0usize;
            for ch in logical.chars() {
                let w = ch.width().unwrap_or(0).max(1);
                if cur_w + w > inner_width {
                    wrapped.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += w;
            }
            wrapped.push(cur);
        }
        let max_lines = max_lines.max(1);
        if wrapped.len() > max_lines {
            wrapped.split_off(wrapped.len() - max_lines)
        } else {
            wrapped
        }
    }

    pub fn footer(&self) -> String {
        if self.multiline {
            "Enter (Ctrl-S submit, Enter newline)  ·  Esc cancel".to_string()
        } else {
            "Enter submit  ·  Esc cancel".to_string()
        }
    }

    /// Render the whole prompt as its own modal.
    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str) {
        let width = dialog_width(area, 70);
        let inner_width = width.saturating_sub(4) as usize;
        let max_lines = 3.max(area.height.saturating_sub(8) as usize);
        let mut lines: Vec<Line<'static>> = self
            .wrapped(inner_width, max_lines)
            .into_iter()
            .map(Line::raw)
            .collect();
        lines.push(Line::default());
        lines.push(hint(&self.footer()));
        render_modal(frame, area, title, color_from_name("cyan"), lines, width);
    }
}
