//! Merging from the Deploy tab: one merge request, or every ready one.
//!
//! Ported from `MergeConfirm` / `MergeAllConfirm` in the Node app's
//! `src/tui/dialogs.js`. Both are irreversible from where they sit, so both
//! spell out exactly what lands where and start their [`InlineChoice`] on
//! Cancel — Enter-mashing through either cancels rather than merging.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::types::DeployTask;
use crate::ui::dialogs::widgets::{bold, coloured, render_confirm, InlineChoice, ListOutcome};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::state::{Action, AppState, MergeTarget};
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// How many merge requests "merge all" names before it stops counting them out.
pub const MERGE_ALL_LISTED: usize = 10;

// --- merge one ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConfirm {
    pub task: Box<DeployTask>,
    pub target: Option<MergeTarget>,
    pub reason: String,
    pub choice: InlineChoice,
}

impl MergeConfirm {
    pub fn new(task: &DeployTask) -> Self {
        let target = crate::ui::deploy::merge_target(task);
        let reason = target.as_ref().err().cloned().unwrap_or_default();
        let target = target.ok();
        let options: Vec<String> = match &target {
            Some(target) => vec!["Cancel".to_string(), format!("Merge !{}", target.iid)],
            None => vec!["Close".to_string()],
        };
        MergeConfirm {
            task: Box::new(task.clone()),
            target,
            reason,
            choice: InlineChoice::new(options),
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            bold(truncate(&self.task.name, 50)),
            coloured(
                format!(
                    "#{}  ·  {}",
                    self.task.id,
                    truncate(&self.task.project_name, 30)
                ),
                "gray",
            ),
            Line::default(),
        ];
        if let Some(mr) = &self.task.mr {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("!{}", mr.iid),
                    Style::default().fg(color_from_name("cyan")),
                ),
                Span::raw(format!(" {}", truncate(&mr.title, 46))),
            ]));
            // Source → target, always: which branch this lands in is the fact
            // the confirmation exists to state.
            lines.push(coloured(
                format!(
                    "{} → {}",
                    truncate(&mr.source_branch, 28),
                    truncate(&mr.target_branch, 20)
                ),
                "gray",
            ));
            if !mr.pipeline.is_empty() {
                lines.push(coloured(format!("pipeline: {}", mr.pipeline), "gray"));
            }
        }
        lines.push(Line::default());
        lines.push(if self.target.is_some() {
            coloured("This merges the MR in GitLab now.", "yellow")
        } else {
            coloured(format!("Blocked: {}", self.reason), "red")
        });
        lines
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.choice.handle_key(key) {
            ListOutcome::Select(1) => match &self.target {
                Some(target) => DialogOutcome::Act(Action::MergeMrs {
                    project: self.task.project_name.clone(),
                    targets: vec![target.clone()],
                }),
                None => DialogOutcome::Close,
            },
            ListOutcome::Select(_) | ListOutcome::Cancel => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border = if self.target.is_some() {
            "yellow"
        } else {
            "red"
        };
        render_confirm(
            frame,
            area,
            " Merge merge request ",
            border,
            &self.lines(),
            &self.choice,
        );
    }
}

// --- merge all --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeAllConfirm {
    pub project: String,
    pub targets: Vec<MergeTarget>,
    /// Merge requests that exist but are not mergeable — counted, not listed:
    /// the reasons are on the rows behind this dialog.
    pub blocked: usize,
    pub choice: InlineChoice,
}

impl MergeAllConfirm {
    pub fn build(project: &str, state: &AppState) -> Self {
        let targets = crate::ui::deploy::ready_targets(state, project);
        let options: Vec<String> = if targets.is_empty() {
            vec!["Close".to_string()]
        } else {
            vec!["Cancel".to_string(), format!("Merge {}", targets.len())]
        };
        MergeAllConfirm {
            project: project.to_string(),
            blocked: crate::ui::deploy::blocked_count(state, project),
            targets,
            choice: InlineChoice::new(options),
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![bold(truncate(&self.project, 50)), Line::default()];
        if self.targets.is_empty() {
            lines.push(coloured("No merge requests are ready to merge.", "yellow"));
        } else {
            let plural = if self.targets.len() == 1 { "" } else { "s" };
            // "one at a time" is a promise: a failure halfway names the MR it
            // failed on rather than leaving a batch in an unknown state.
            lines.push(coloured(
                format!(
                    "Merging {} merge request{plural}, one at a time:",
                    self.targets.len()
                ),
                "yellow",
            ));
            for target in self.targets.iter().take(MERGE_ALL_LISTED) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  !{}", target.iid),
                        Style::default().fg(color_from_name("cyan")),
                    ),
                    Span::styled(
                        format!(" #{} ", target.task_id),
                        Style::default().fg(color_from_name("gray")),
                    ),
                    Span::raw(truncate(&target.name, 38)),
                ]));
            }
            if self.targets.len() > MERGE_ALL_LISTED {
                lines.push(coloured(
                    format!("  …and {} more", self.targets.len() - MERGE_ALL_LISTED),
                    "gray",
                ));
            }
        }
        if self.blocked > 0 {
            let plural = if self.blocked == 1 { "" } else { "s" };
            lines.push(Line::default());
            lines.push(coloured(
                format!("{} other MR{plural} still blocked — skipped.", self.blocked),
                "gray",
            ));
        }
        lines
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.choice.handle_key(key) {
            ListOutcome::Select(1) if !self.targets.is_empty() => {
                DialogOutcome::Act(Action::MergeMrs {
                    project: self.project.clone(),
                    targets: self.targets.clone(),
                })
            }
            ListOutcome::Select(_) | ListOutcome::Cancel => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border = if self.targets.is_empty() {
            "gray"
        } else {
            "yellow"
        };
        render_confirm(
            frame,
            area,
            " Merge all ready ",
            border,
            &self.lines(),
            &self.choice,
        );
    }
}
