//! `?`: every key for the view you are on, as you have them now.
//!
//! Built from [`crate::ui::keymap`] when it opens, so it shows your remaps and
//! only the keys your role is offered. The status bar has room for a few keys;
//! this is where the rest are.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal};
use crate::ui::dialogs::DialogOutcome;
use crate::ui::keymap::{help_sections, Keymap};
use crate::ui::state::View;
use crate::ui::theme::color_from_name;

/// One line of the help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Heading(&'static str),
    Key(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpDialog {
    pub title: String,
    pub lines: Vec<HelpLine>,
    /// The settings key as the person has it, for the footer.
    pub settings_key: String,
    /// The first line shown.
    pub scroll: usize,
}

/// The rows the frame, the title and the footer take.
const CHROME: usize = 4;

impl HelpDialog {
    pub fn new(keymap: &Keymap, view: View) -> Self {
        let mut lines = Vec::new();
        for (title, keys) in help_sections(keymap, view) {
            if keys.is_empty() {
                continue;
            }
            if !lines.is_empty() {
                lines.push(HelpLine::Heading(""));
            }
            lines.push(HelpLine::Heading(title));
            lines.extend(keys.into_iter().map(|(key, help)| HelpLine::Key(key, help)));
        }
        HelpDialog {
            title: format!(" Keys · {} role ", keymap.role().as_str().to_uppercase()),
            lines,
            settings_key: keymap.key("global.settings"),
            scroll: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogOutcome {
        let last = self.lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(last),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + 10).min(last),
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => return DialogOutcome::Close,
            _ => {}
        }
        DialogOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 80);
        let key_width = self
            .lines
            .iter()
            .filter_map(|line| match line {
                HelpLine::Key(key, _) => Some(key.chars().count()),
                HelpLine::Heading(_) => None,
            })
            .max()
            .unwrap_or(1)
            .max(5);
        let room = (area.height as usize).saturating_sub(CHROME).max(1);
        // Scrolled no further than the last full page.
        let top = self.scroll.min(self.lines.len().saturating_sub(room));
        let cyan = Style::default().fg(color_from_name("cyan"));
        let mut lines: Vec<Line<'static>> = self
            .lines
            .iter()
            .skip(top)
            .take(room)
            .map(|line| match line {
                HelpLine::Heading(title) => Line::from(Span::styled(title.to_string(), cyan)),
                HelpLine::Key(key, help) => Line::from(vec![
                    Span::styled(
                        format!("  {key:<key_width$}  "),
                        Style::default().fg(color_from_name("yellow")),
                    ),
                    Span::raw(help.clone()),
                ]),
            })
            .collect();
        lines.push(Line::default());
        let change = format!("{} then Tab to Keys to change them", self.settings_key);
        lines.push(hint(&if self.lines.len() > room {
            format!("↑↓ scroll  {change}  Esc close")
        } else {
            format!("{change}  Esc close")
        }));
        render_modal(
            frame,
            area,
            &self.title,
            color_from_name("cyan"),
            lines,
            width,
        );
    }
}
