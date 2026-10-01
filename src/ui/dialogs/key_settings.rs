//! The settings dialog's Keys page: give a key a different letter.
//!
//! It lists the keys your role is offered, by where they work. Enter waits for
//! the new key, and the key table refuses one that would clash (see
//! [`Keymap::refuse`]). Backspace puts the default back. Each change saves at
//! once, like the other pages, and applies to the next key you press.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::ConfigHandle;
use crate::ui::dialogs::widgets::hint;
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::keymap::{Binding, Key, Keymap, Scope};
use crate::ui::theme::color_from_name;

use super::settings::SettingsDialog;

/// The scopes in the order the page lists them.
const SCOPES: [Scope; 5] = [
    Scope::Global,
    Scope::Sessions,
    Scope::Conversation,
    Scope::Board,
    Scope::Deploy,
];

/// The widest a key's description is drawn.
const LABEL_WIDTH: usize = 50;

/// The keys the page lists for the role, in the order `↑↓` walks them: the
/// ones that can be remapped, by scope.
pub fn bindings(config: &ConfigHandle) -> Vec<&'static Binding> {
    let keymap = keymap(config);
    SCOPES
        .iter()
        .flat_map(|scope| keymap.offered(*scope).collect::<Vec<_>>())
        .filter(|binding| !binding.fixed)
        .collect()
}

fn keymap(config: &ConfigHandle) -> Keymap {
    Keymap::new(config.role(), config.key_overrides())
}

fn key_text(key: Key) -> String {
    match key {
        Key::Char(ch) => ch.to_string(),
        Key::Named(name) => name.to_string(),
    }
}

impl SettingsDialog {
    pub(super) fn keys_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let rows = bindings(ctx.config);
        let Some(binding) = rows
            .get(self.row.min(rows.len().saturating_sub(1)))
            .copied()
        else {
            return DialogOutcome::Close;
        };

        // Waiting for the new key.
        if self.editing.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.notice = None;
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.editing = None;
                    self.notice = Some(assign(ctx.config, binding, ch));
                }
                _ => {}
            }
            return DialogOutcome::Stay;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.row = (self.row + 1).min(rows.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                self.editing = Some(String::new());
                self.notice = Some(format!(
                    "Press the new key for \"{}\". Esc keeps the old one.",
                    binding.help
                ));
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.notice = Some(match ctx.config.set_key(binding.id, None) {
                    Ok(()) => format!(
                        "\"{}\" is back on `{}`.",
                        binding.help,
                        key_text(binding.key)
                    ),
                    Err(error) => format!("Could not save that: {error}"),
                });
            }
            KeyCode::Esc | KeyCode::Char('s' | 'q' | ',') => return DialogOutcome::Close,
            _ => {}
        }
        DialogOutcome::Stay
    }

    /// The page, scrolled so the selected key is on it. `height` is the rows
    /// the page may take.
    pub(super) fn keys_lines(&self, config: &ConfigHandle, height: usize) -> Vec<Line<'static>> {
        let keymap = keymap(config);
        let mut lines = vec![hint(&format!(
            "The {} role's keys. Enter sets a new key, Backspace puts the default back.",
            config.role().as_str().to_uppercase()
        ))];
        let mut body: Vec<Line<'static>> = Vec::new();
        let mut selected_line = 0;
        let mut index = 0usize;
        for scope in SCOPES {
            let offered: Vec<&Binding> = keymap.offered(scope).filter(|b| !b.fixed).collect();
            if offered.is_empty() {
                continue;
            }
            body.push(hint(&format!("──── {} ────", scope.title())));
            for binding in offered {
                let selected = index == self.row;
                if selected {
                    selected_line = body.len();
                }
                body.push(self.key_line(&keymap, binding, selected));
                index += 1;
            }
        }
        // Scrolled so the selected row stays in view, a third of the way down.
        let room = height.saturating_sub(lines.len()).max(1);
        let top = selected_line
            .saturating_sub(room / 3)
            .min(body.len().saturating_sub(room));
        lines.extend(body.into_iter().skip(top).take(room));
        lines
    }

    fn key_line(&self, keymap: &Keymap, binding: &Binding, selected: bool) -> Line<'static> {
        let label: String = binding.help.chars().take(LABEL_WIDTH).collect();
        let style = if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let value = if selected && self.editing.is_some() {
            "…".to_string()
        } else {
            keymap.label(binding)
        };
        let mut spans = vec![
            Span::styled(
                if selected { "› " } else { "  " }.to_string(),
                Style::default().fg(color_from_name("cyan")),
            ),
            Span::styled(format!("{label:<LABEL_WIDTH$} "), style),
            Span::styled(format!("[{value}]"), style),
        ];
        if keymap.key_of(binding) != binding.key {
            spans.push(Span::styled(
                format!("  default {}", key_text(binding.key)),
                Style::default().fg(color_from_name("gray")),
            ));
        }
        Line::from(spans)
    }
}

/// Give `binding` the key `ch`, or say why not. Pressing its default puts the
/// default back, so the file keeps only real changes.
fn assign(config: &mut ConfigHandle, binding: &Binding, ch: char) -> String {
    if let Some(reason) = keymap(config).refuse(binding.id, ch) {
        return reason;
    }
    let remap = (binding.key != Key::Char(ch)).then_some(ch);
    match config.set_key(binding.id, remap) {
        Ok(()) => format!("\"{}\" is now `{ch}`.", binding.help),
        Err(error) => format!("Could not save that: {error}"),
    }
}
