//! The two confirmations that can stop a launch.
//!
//! Ported from `BlockedBy` / `AlreadyRunning` in the Node app's
//! `src/tui/dialogs.js`. Both are incident reports rendered as dialogs, and
//! both offer the override rather than refusing flatly — Odoo is sometimes
//! behind reality, and a second agent in a different worktree is occasionally
//! what you want.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::odoo::Blocker;
use crate::ui::board::{RacingSession, StartRequest};
use crate::ui::dialogs::widgets::{dialog_width, gray, hint, render_modal};
use crate::ui::dialogs::{BoardData, DialogCtx, DialogOutcome, Remote, TaskCommand};
use crate::ui::theme::color_from_name;
use crate::util::{time_ago, truncate};

use super::board::TaskAction;

/// Odoo says this task is blocked. Show by what, and let the decision be made
/// with the facts rather than refusing flatly — sometimes the blocker is
/// effectively done and Odoo has not caught up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedBy {
    pub request: Box<StartRequest>,
    pub open: u32,
    pub blockers: Remote<Vec<Blocker>>,
}

impl BlockedBy {
    pub fn new(request: StartRequest, open: u32) -> Self {
        BlockedBy {
            request: Box::new(request),
            open,
            blockers: Remote::Loading,
        }
    }

    pub fn accept(&mut self, data: &BoardData) -> bool {
        match data {
            BoardData::Blockers { task_id, blockers } if *task_id == self.request.task.id => {
                self.blockers = Remote::Ready(blockers.clone());
                true
            }
            BoardData::Failed { task_id, error } if *task_id == Some(self.request.task.id) => {
                self.blockers = Remote::Failed(error.clone());
                true
            }
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                DialogOutcome::Start(Box::new(self.request.as_ref().clone().forced()))
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                DialogOutcome::Close
            }
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let red = color_from_name("red");
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled(
                format!(
                    "#{} {}",
                    self.request.task.id,
                    truncate(&self.request.task.name, 46)
                ),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(
                "Odoo says this is blocked by work that has not landed:",
                Style::default().fg(red),
            )),
            Line::default(),
        ];
        match &self.blockers {
            Remote::Loading => lines.push(hint("loading blockers…")),
            // Never leave the placeholder up: with no Odoo reachable the dialog
            // still has to say something useful.
            Remote::Failed(error) => {
                lines.push(hint(&format!("{} open blocker(s): {error}", self.open)))
            }
            Remote::Ready(blockers) if blockers.is_empty() => lines.push(hint(&format!(
                "{} open blocker(s), details unavailable.",
                self.open
            ))),
            Remote::Ready(blockers) => {
                for blocker in blockers {
                    let (mark, colour) = if blocker.done {
                        ("✓", "green")
                    } else {
                        ("✗", "red")
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {mark} "),
                            Style::default().fg(color_from_name(colour)),
                        ),
                        Span::raw(format!("#{} {}", blocker.id, truncate(&blocker.name, 40))),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("     {}", blocker.stage_name),
                        gray(),
                    )));
                }
            }
        }
        lines.push(Line::default());
        lines.push(hint("Starting now means building against something that"));
        lines.push(hint("does not exist yet."));
        lines.push(Line::default());
        lines.push(hint("y start anyway    n / Esc cancel"));
        render_modal(frame, area, " Blocked ", red, lines, dialog_width(area, 66));
    }
}

/// A session is already working this task.
///
/// Nine agents once raced on one task in one working tree because nothing
/// checked. Starting another is occasionally what you want — a different
/// worktree, or the first is wedged — so this shows what is already running and
/// lets you decide, rather than refusing or silently piling on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlreadyRunning {
    pub request: Box<StartRequest>,
    pub running: Vec<RacingSession>,
}

impl AlreadyRunning {
    pub fn new(request: StartRequest, running: Vec<RacingSession>) -> Self {
        AlreadyRunning {
            request: Box::new(request),
            running,
        }
    }

    /// The expensive case: two agents in the same checkout overwrite each
    /// other's branch and files. Different worktrees merely duplicate work.
    pub fn shares_a_worktree(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        !self.running.iter().all(|s| seen.insert(s.cwd.as_str()))
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                DialogOutcome::Start(Box::new(self.request.as_ref().clone().forced()))
            }
            KeyCode::Char('g') | KeyCode::Char('G') => DialogOutcome::Task(Box::new(TaskCommand {
                task: self.request.task.clone(),
                action: TaskAction::FocusTerminal,
            })),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                DialogOutcome::Close
            }
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let same_tree = self.shares_a_worktree();
        let border = color_from_name(if same_tree { "red" } else { "yellow" });
        let count = self.running.len();
        let verb = if count == 1 { " is" } else { "s are" };
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled(
                format!(
                    "#{} {}",
                    self.request.task.id,
                    truncate(&self.request.task.name, 44)
                ),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
            Line::from(Span::styled(
                format!("{count} session{verb} already working this task:"),
                Style::default().fg(color_from_name("yellow")),
            )),
            Line::default(),
        ];
        for session in self.running.iter().take(5) {
            let age = session
                .last_timestamp
                .as_deref()
                .and_then(crate::util::parse_timestamp)
                .map(|ts| time_ago(ts, chrono::Utc::now()))
                .unwrap_or_else(|| "unknown".to_string());
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}", crate::ui::board::short(&session.session_id)),
                    Style::default().fg(color_from_name("cyan")),
                ),
                Span::raw(format!("  {:?}  ", session.status)),
                Span::styled(age, gray()),
            ]));
            lines.push(Line::from(Span::styled(
                format!("     {}", truncate(&session.cwd, 46)),
                gray(),
            )));
        }
        if count > 5 {
            lines.push(hint(&format!("  …and {} more", count - 5)));
        }
        lines.push(Line::default());
        if same_tree {
            lines.push(Line::from(Span::styled(
                "Two of these share a working tree — they will fight",
                Style::default().fg(color_from_name("red")),
            )));
            lines.push(Line::from(Span::styled(
                "over the same branch and files.",
                Style::default().fg(color_from_name("red")),
            )));
        } else {
            lines.push(hint(
                "Another agent on the same task will duplicate its work.",
            ));
        }
        lines.push(Line::default());
        lines.push(hint(
            "g go to the running one    y start another    n / Esc cancel",
        ));
        render_modal(
            frame,
            area,
            " Already running ",
            border,
            lines,
            dialog_width(area, 66),
        );
    }
}
