//! The open-merge-request browser, from the board tab's `M`.
//!
//! Ported from `OpenMRs` in the Node app's `src/tui/dialogs.js`. Not a picker:
//! Enter opens one in a browser and the dialog STAYS OPEN, so a morning's worth
//! of review links can be opened in a row.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::gitlab::OpenMr;
use crate::ui::dialogs::widgets::{
    coloured, dialog_width, hint, render_modal, ListOutcome, SelectList,
};
use crate::ui::dialogs::{BoardData, DialogCtx, DialogOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;
use crate::util::truncate;

// --- open merge requests ----------------------------------------------------------------

/// The current user's open merge requests, from the board tab's `M`.
///
/// Enter opens one in a browser and the dialog STAYS OPEN, so several can be
/// opened in a row — the Node behaviour, and the reason this is a browser
/// rather than a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenMrs {
    pub mrs: Option<Vec<OpenMr>>,
    pub error: Option<String>,
    pub list: SelectList,
}

impl Default for OpenMrs {
    fn default() -> Self {
        OpenMrs::loading()
    }
}

impl OpenMrs {
    pub fn loading() -> Self {
        OpenMrs {
            mrs: None,
            error: None,
            list: SelectList::default(),
        }
    }

    /// Take the answer the worker fetched. `false` means it was not ours.
    pub fn accept(&mut self, data: &BoardData) -> bool {
        match data {
            BoardData::OpenMrs(mrs) => {
                self.list = SelectList::new(mrs.iter().map(row).collect());
                self.mrs = Some(mrs.clone());
                true
            }
            BoardData::Failed {
                task_id: None,
                error,
            } => {
                self.error = Some(error.clone());
                true
            }
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let Some(mrs) = &self.mrs else {
            // Nothing to steer yet: any key closes rather than appearing stuck.
            return DialogOutcome::Close;
        };
        match self.list.handle_key(key) {
            ListOutcome::Select(index) => match mrs.get(index) {
                Some(mr) if !mr.url.is_empty() => DialogOutcome::Keep {
                    action: Some(Action::OpenUrl(mr.url.clone())),
                    flash: None,
                },
                _ => DialogOutcome::Stay,
            },
            ListOutcome::Cancel => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 74);
        if let Some(error) = &self.error {
            return render_modal(
                frame,
                area,
                " Open MRs ",
                color_from_name("red"),
                vec![coloured(error.clone(), "red"), hint("Esc to close")],
                width,
            );
        }
        let Some(mrs) = &self.mrs else {
            return render_modal(
                frame,
                area,
                " Open MRs ",
                color_from_name("cyan"),
                vec![hint("Scanning your open merge requests…")],
                width,
            );
        };
        if mrs.is_empty() {
            return render_modal(
                frame,
                area,
                " Open MRs ",
                color_from_name("cyan"),
                vec![
                    hint("No open merge requests authored by you. 🎉"),
                    hint("Esc to close"),
                ],
                width,
            );
        }
        let title = format!(" Your open merge requests ({}) ", mrs.len());
        let mut lines = self.list.lines(area.height);
        lines.push(hint("Enter open in browser  ·  Esc close"));
        render_modal(frame, area, &title, color_from_name("cyan"), lines, width);
    }
}

fn row(mr: &OpenMr) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("!{}", mr.iid),
        Style::default().fg(color_from_name("cyan")),
    )];
    spans.push(Span::raw("  "));
    if mr.draft {
        spans.push(Span::styled(
            "[draft] ",
            Style::default().fg(color_from_name("yellow")),
        ));
    }
    spans.push(Span::raw(truncate(&mr.title, 38)));
    if !mr.project.is_empty() {
        let leaf = mr.project.rsplit('/').next().unwrap_or(&mr.project);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            truncate(leaf, 18),
            Style::default().fg(color_from_name("magenta")),
        ));
    }
    if !mr.target_branch.is_empty() {
        spans.push(Span::styled(
            format!("  → {}", truncate(&mr.target_branch, 14)),
            Style::default().fg(color_from_name("gray")),
        ));
    }
    Line::from(spans)
}
