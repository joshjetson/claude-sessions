//! Shift-Q: stop the background daemon as well as the dashboard.
//!
//! Ported from `ShutdownConfirm` in the Node app's `src/tui/dialogs.js`, and
//! pinned by `test/shutdown.test.js`. Plain `q` deliberately leaves the daemon
//! running — that is what lets a deploy outlive the TUI — so the full stop has
//! to confirm, and has to say what it would actually kill.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::types::Session;
use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal};
use crate::ui::dialogs::DialogOutcome;
use crate::ui::state::Quit;
use crate::ui::theme::color_from_name;

/// How many of each list the dialog names before it stops. Enough to recognise
/// what is at stake; not so many that the dialog scrolls.
const MAX_DEPLOYS_LISTED: usize = 4;
const MAX_SESSIONS_LISTED: usize = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownConfirm {
    /// Deploy runs that would be killed — the ones actually RUNNING, never
    /// padded with finished ones, or the warning becomes noise nobody reads.
    pub running_deploys: Vec<String>,
    /// Task ids with a live session. These SURVIVE — they are separate
    /// processes and the daemon only watches them.
    pub task_sessions: Vec<i64>,
}

impl ShutdownConfirm {
    /// Task sessions are derived from the live scan rather than from an
    /// in-memory link table, so the list is right immediately after a daemon
    /// restart — the same reason the board marks live tasks from transcripts.
    pub fn from_sessions<'a>(sessions: impl Iterator<Item = &'a Session>) -> Self {
        let mut task_sessions: Vec<i64> = sessions.filter_map(|s| s.task_id).collect();
        task_sessions.sort_unstable();
        task_sessions.dedup();
        ShutdownConfirm {
            running_deploys: Vec::new(),
            task_sessions,
        }
    }

    /// The deploys the engine is supervising right now.
    pub fn with_deploys(mut self, running: Vec<String>) -> Self {
        self.running_deploys = running;
        self
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogOutcome {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => DialogOutcome::Quit(Quit::ShutdownAll),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                DialogOutcome::Close
            }
            _ => DialogOutcome::Stay,
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let red = ratatui::style::Style::default().fg(color_from_name("red"));
        let mut lines = vec![
            Line::default(),
            Line::raw("Stop the background daemon and quit?"),
            Line::default(),
        ];

        if !self.running_deploys.is_empty() {
            let n = self.running_deploys.len();
            let (plural, them) = if n == 1 { ("", "it") } else { ("s", "them") };
            lines.push(Line::from(Span::styled(
                format!("⚠ {n} deploy{plural} still running — stopping now kills {them}:"),
                red,
            )));
            for name in self.running_deploys.iter().take(MAX_DEPLOYS_LISTED) {
                lines.push(Line::from(Span::styled(format!("    {name}"), red)));
            }
            lines.push(Line::default());
        }

        if !self.task_sessions.is_empty() {
            let n = self.task_sessions.len();
            let plural = if n == 1 { "" } else { "s" };
            let ids = self
                .task_sessions
                .iter()
                .take(MAX_SESSIONS_LISTED)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", #");
            lines.push(hint(&format!(
                "{n} task session{plural} being tracked (#{ids})."
            )));
            lines.push(hint("Those keep running — the daemon only watches them."));
            lines.push(Line::default());
        }

        if self.running_deploys.is_empty() && self.task_sessions.is_empty() {
            lines.push(hint("Nothing is running. Safe to stop."));
            lines.push(Line::default());
        }

        lines.push(hint("q on its own just closes the dashboard and leaves"));
        lines.push(hint("the daemon working in the background."));
        lines.push(Line::default());
        lines.push(hint("y stop everything    n / Esc cancel"));
        lines
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border = if self.running_deploys.is_empty() {
            color_from_name("yellow")
        } else {
            color_from_name("red")
        };
        render_modal(
            frame,
            area,
            " Shut down ",
            border,
            self.lines(),
            dialog_width(area, 64),
        );
    }
}
