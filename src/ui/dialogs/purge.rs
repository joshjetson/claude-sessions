//! `X` — close out every finished session in one go.
//!
//! Ported from `PurgeConfirm` in the Node app's `src/tui/dialogs.js`. The
//! decision itself is [`crate::purge`], which is pure; this is the confirmation
//! that shows what would happen and why each survivor survives.
//!
//! Stages come from the board where it has them and from Odoo for the rest: the
//! board holds only what the current filter shows, and reading absence as
//! "finished" would kill live agents belonging to somebody else's task.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::purge::{group_kept, group_purged, plan_purge, KeepReason, PurgePlan, PurgeTarget};
use crate::ui::dialogs::widgets::{dialog_width, gray, hint, render_modal};
use crate::ui::dialogs::{BoardData, DialogCtx, DialogOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// How many kept groups the dialog lists before it stops. Node showed six.
const KEPT_GROUPS_SHOWN: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeConfirm {
    targets: Vec<PurgeTarget>,
    /// Task id -> stage name. Filled from the board up front and topped up from
    /// Odoo for anything the board did not have.
    stages: BTreeMap<i64, String>,
    /// Task ids still being looked up. While this is non-empty the dialog says
    /// so rather than offering to purge on half the answer.
    pending: Vec<i64>,
    include_review: bool,
    error: Option<String>,
}

impl PurgeConfirm {
    /// `known` is what the board already knows; anything missing is requested.
    pub fn new(targets: Vec<PurgeTarget>, known: BTreeMap<i64, String>) -> Self {
        let pending: Vec<i64> = targets
            .iter()
            .filter_map(|target| target.task_id)
            .filter(|task_id| !known.contains_key(task_id))
            .collect();
        PurgeConfirm {
            targets,
            stages: known,
            pending,
            include_review: false,
            error: None,
        }
    }

    /// Task ids whose stage still has to be fetched, deduplicated.
    pub fn missing_stages(&self) -> Vec<i64> {
        let mut ids = self.pending.clone();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn plan(&self) -> PurgePlan {
        plan_purge(
            &self.targets,
            |task_id| self.stages.get(&task_id).cloned(),
            self.include_review,
        )
    }

    /// Fold in a stage lookup. Returns false when the answer was not this
    /// dialog's, which is what [`crate::ui::dialogs::Dialog::accept`] checks.
    pub fn accept(&mut self, data: &BoardData) -> bool {
        match data {
            BoardData::TaskStages(found) => {
                for (task_id, stage) in found.iter() {
                    self.stages.insert(*task_id, stage.clone());
                }
                self.pending.clear();
                true
            }
            // A failure is not fatal: what the board knew still stands, and
            // every task with no stage is kept, which is the safe direction.
            BoardData::Failed {
                task_id: None,
                error,
            } => {
                self.error = Some(error.clone());
                self.pending.clear();
                true
            }
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match key.code {
            // `q` closes, as it does in every other dialog: a key that quits
            // the tool everywhere else must not quietly change what a purge
            // will kill.
            KeyCode::Esc | KeyCode::Char('q') => DialogOutcome::Close,
            KeyCode::Char(' ') => {
                self.include_review = !self.include_review;
                DialogOutcome::Stay
            }
            KeyCode::Enter => {
                let plan = self.plan();
                if plan.purge.is_empty() || !self.pending.is_empty() {
                    return DialogOutcome::Close;
                }
                DialogOutcome::Act(Action::Purge(Box::new(plan.purge)))
            }
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 74);
        if !self.pending.is_empty() {
            render_modal(
                frame,
                area,
                " Purge sessions ",
                color_from_name("cyan"),
                vec![hint("Working out which sessions are finished…")],
                width,
            );
            return;
        }

        let plan = self.plan();
        let inner = width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = vec![Line::default()];

        if plan.purge.is_empty() {
            lines.push(hint("Nothing to purge — no finished sessions."));
        } else {
            lines.push(bold(&format!(
                "Close {} finished session(s):",
                plan.purge.len()
            )));
            for group in group_purged(&plan.purge) {
                let ids: Vec<String> = group
                    .entries
                    .iter()
                    .map(|entry| match entry.target.task_id {
                        Some(task_id) => format!("#{task_id}"),
                        None => "#?".to_string(),
                    })
                    .collect();
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        group.stage.clone(),
                        Style::default().fg(color_from_name("red")),
                    ),
                    Span::raw("  "),
                    Span::raw(truncate(
                        &ids.join(" "),
                        inner.saturating_sub(group.stage.chars().count() + 4),
                    )),
                ]));
            }
        }

        lines.push(Line::default());
        let kept = group_kept(&plan.keep);
        if !kept.is_empty() {
            lines.push(bold(&format!("Keeping {}:", plan.keep.len())));
            for group in kept.iter().take(KEPT_GROUPS_SHOWN) {
                // The group name already carries it for untracked sessions.
                let reason = group.entries.first().map(|entry| entry.reason);
                let why = match reason {
                    Some(KeepReason::NoTask) | None => String::new(),
                    Some(reason) => format!(" ({})", reason.label()),
                };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        group.stage.clone(),
                        Style::default().fg(color_from_name("green")),
                    ),
                    Span::raw(format!(" × {}", group.entries.len())),
                    Span::styled(why, gray()),
                ]));
            }
        }

        lines.push(Line::default());
        lines.push(if self.include_review {
            Line::from(Span::styled(
                "[x] QA/UAT/Staging INCLUDED — you lose their live scrollback".to_string(),
                Style::default().fg(color_from_name("yellow")),
            ))
        } else {
            hint("[ ] QA/UAT/Staging kept — Space to purge those too")
        });
        if let Some(error) = &self.error {
            lines.push(Line::from(Span::styled(
                format!(
                    "Some stages came from the board only: {}",
                    truncate(error, inner.saturating_sub(34))
                ),
                Style::default().fg(color_from_name("yellow")),
            )));
        }
        lines.push(hint(
            "Transcripts stay archived; v on the task reopens the conversation.",
        ));
        lines.push(Line::default());
        lines.push(hint(if plan.purge.is_empty() {
            "Space QA/UAT    Esc/q close"
        } else {
            "Enter purge    Space QA/UAT    Esc/q cancel"
        }));

        let border = if plan.purge.is_empty() {
            color_from_name("cyan")
        } else {
            color_from_name("red")
        };
        render_modal(frame, area, " Purge sessions ", border, lines, width);
    }
}

fn bold(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}
