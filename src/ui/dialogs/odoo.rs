//! Two dialogs that wait on an Odoo answer: the stage picker and the
//! notification menu. The project filter is the third and lives in
//! [`super::project_filter`].
//!
//! Ported from `StagePicker` / `NotifMenu` in the Node app's
//! `src/tui/dialogs.js`. Node fetched inside a `useEffect` and rendered
//! "Loading…" until it resolved; here the lookup is an [`crate::ui::state::Action`]
//! and the answer arrives as [`BoardData`], so the draw thread never waits on a
//! round trip. [`Remote`] is the three states that produces.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::odoo::StageRecord;
use crate::types::{Notification, NotificationStatus};
use crate::ui::board::SessionTarget;
use crate::ui::dialogs::widgets::{
    dialog_width, gray, hint, render_modal, ListOutcome, SelectList,
};
use crate::ui::dialogs::{BoardData, DialogCtx, DialogOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// Something fetched off the UI thread. `Failed` is a real state, not an
/// absence: a dialog must say what went wrong rather than sit on "loading".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// Move a task to a stage the user names. Nothing is resolved and nothing is
/// guessed — this is the one move where the answer is not a policy decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePicker {
    pub task_id: i64,
    pub task_name: String,
    pub current_stage_id: i64,
    pub stages: Remote<Vec<StageRecord>>,
    pub list: SelectList,
}

impl StagePicker {
    pub fn new(task: &crate::types::Task) -> Self {
        StagePicker {
            task_id: task.id,
            task_name: task.name.clone(),
            current_stage_id: task.stage_id,
            stages: Remote::Loading,
            list: SelectList::default(),
        }
    }

    pub fn accept(&mut self, data: &BoardData) -> bool {
        match data {
            BoardData::Stages { task_id, stages } if *task_id == self.task_id => {
                self.list = SelectList::new(
                    stages
                        .iter()
                        .map(|stage| {
                            if stage.id == self.current_stage_id {
                                Line::from(vec![
                                    Span::styled(
                                        "● ",
                                        ratatui::style::Style::default()
                                            .fg(color_from_name("green")),
                                    ),
                                    Span::raw(stage.name.clone()),
                                    Span::styled("  (current)".to_string(), gray()),
                                ])
                            } else {
                                Line::raw(format!("  {}", stage.name))
                            }
                        })
                        .collect(),
                );
                // Open on the stage the task is in, so ↑↓ moves relative to it.
                self.list.sel = stages
                    .iter()
                    .position(|stage| stage.id == self.current_stage_id)
                    .unwrap_or(0);
                self.stages = Remote::Ready(stages.clone());
                true
            }
            BoardData::Failed { task_id, error } if *task_id == Some(self.task_id) => {
                self.stages = Remote::Failed(error.clone());
                true
            }
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let Remote::Ready(stages) = &self.stages else {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => DialogOutcome::Close,
                _ => DialogOutcome::Stay,
            };
        };
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match stages.get(index) {
                Some(stage) if stage.id != self.current_stage_id => {
                    DialogOutcome::Act(Action::MoveStage {
                        task_id: self.task_id,
                        stage_id: stage.id,
                        stage_name: stage.name.clone(),
                    })
                }
                _ => DialogOutcome::Close,
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" Move #{} to stage ", self.task_id);
        let width = dialog_width(area, 60);
        let lines = match &self.stages {
            Remote::Loading => vec![hint("Loading stages…")],
            Remote::Failed(error) => vec![
                Line::from(Span::styled(
                    error.clone(),
                    ratatui::style::Style::default().fg(color_from_name("red")),
                )),
                hint("Esc to close"),
            ],
            Remote::Ready(_) => {
                let mut lines = self.list.lines(area.height);
                lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
                lines
            }
        };
        render_modal(frame, area, &title, color_from_name("cyan"), lines, width);
    }
}

/// What to do about one notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifMenu {
    pub notification: Box<Notification>,
    /// The live session that produced it, when one could be resolved. "Go to
    /// session" is offered only then — an entry that cannot work is worse than
    /// no entry.
    pub target: Option<SessionTarget>,
    pub entries: Vec<NotifAction>,
    pub list: SelectList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifAction {
    GoToSession,
    Resolve,
    Dismiss,
    Cancel,
}

impl NotifMenu {
    pub fn new(
        notification: Notification,
        live: Option<(String, Option<std::path::PathBuf>, String)>,
    ) -> Self {
        let target = live.map(|(session_id, session_file, cwd)| SessionTarget::Session {
            session_id,
            session_file,
            project: crate::util::project_name(&cwd),
        });
        let mut entries = Vec::new();
        let mut rows = Vec::new();
        if target.is_some() {
            entries.push(NotifAction::GoToSession);
            rows.push(Line::raw("→  Go to session"));
        }
        entries.push(NotifAction::Resolve);
        rows.push(Line::raw("✓  Resolve"));
        entries.push(NotifAction::Dismiss);
        rows.push(Line::raw("✕  Dismiss"));
        entries.push(NotifAction::Cancel);
        rows.push(Line::raw("·  Cancel"));
        NotifMenu {
            notification: Box::new(notification),
            target,
            entries,
            list: SelectList::new(rows),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.entries.get(index) {
                Some(NotifAction::GoToSession) => match &self.target {
                    Some(target) => DialogOutcome::GoTo(Box::new(target.clone())),
                    None => DialogOutcome::Close,
                },
                Some(NotifAction::Resolve) => DialogOutcome::Act(Action::Notifications {
                    ids: vec![self.notification.id.clone()],
                    status: Some(NotificationStatus::Resolved),
                }),
                Some(NotifAction::Dismiss) => DialogOutcome::Act(Action::Notifications {
                    ids: vec![self.notification.id.clone()],
                    status: None,
                }),
                _ => DialogOutcome::Close,
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" 🔔 {} ", truncate(&self.notification.title, 46));
        let mut lines = self.list.lines(area.height);
        lines.push(hint(&truncate(&self.notification.message, 60)));
        render_modal(
            frame,
            area,
            &title,
            color_from_name("cyan"),
            lines,
            dialog_width(area, 64),
        );
    }
}
