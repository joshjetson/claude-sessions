//! The task action menu, and the prompt for typing context before a launch.
//!
//! Ported from `TaskMenu` / `ContextDialog` in the Node app's
//! `src/tui/dialogs.js`. Neither launches anything: each returns a
//! [`StartRequest`] and the one start flow in [`crate::ui::board::start`] runs
//! the guards, resolves the folder and builds the prompt — so a new menu entry
//! cannot accidentally skip a guard. The guards themselves are in
//! [`super::gates`].

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::types::Task;
use crate::ui::board::{LaunchKind, StartRequest};
use crate::ui::dialogs::widgets::{
    dialog_width, hint, render_modal, ListOutcome, PromptOutcome, SelectList, TextPrompt,
};
use crate::ui::dialogs::{DialogCtx, DialogOutcome, TaskCommand};
use crate::ui::state::AppState;
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// What selecting a task-menu row does. Kept as data so the menu can be
/// asserted on without driving keys through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAction {
    GoToSession,
    FocusTerminal,
    Start(LaunchKind),
    /// Type something first, then start or resume.
    Context {
        revision: bool,
    },
    MoveStage,
    OpenInBrowser,
    RepoFolders,
    TargetBranch,
    /// Named but not yet built — says when it arrives rather than doing nothing.
    Later(&'static str),
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMenu {
    pub task: Box<Task>,
    pub entries: Vec<(String, TaskAction)>,
    pub list: SelectList,
}

impl TaskMenu {
    pub fn build(task: &Task, state: &AppState) -> Self {
        let archived = state.board.archived_tasks.contains(&task.id);
        let live =
            crate::ui::board::task_session(state.sessions(), task.id, state.board.link(task.id))
                .is_some();

        let mut entries: Vec<(String, TaskAction)> = Vec::new();
        // A running session is surfaced first: it is the answer to "what is
        // happening with this" more often than anything below it.
        if live {
            entries.push((
                "👁  View live session (conversation)".into(),
                TaskAction::GoToSession,
            ));
            entries.push(("⌨  Open session terminal".into(), TaskAction::FocusTerminal));
        }
        entries.push(("▶  Start task".into(), TaskAction::Start(LaunchKind::Task)));
        entries.push((
            "✎  Add context & start…".into(),
            TaskAction::Context { revision: false },
        ));
        // The QA label states what selecting it will do — start, resume a
        // round, or open a new one — which depends on QAden run state the board
        // cannot otherwise show. That reader lands with the extras phase; until
        // then the label says so rather than implying a round is in progress.
        entries.push((
            "🧪  QA — round state lands with the extras phase".into(),
            TaskAction::Start(LaunchKind::Qa),
        ));
        entries.push((
            "🧪  QA (dry run — report only, no note)".into(),
            TaskAction::Start(LaunchKind::QaDry),
        ));
        entries.push((
            "🔎  Pre-work brief (Optics)".into(),
            TaskAction::Start(LaunchKind::PreOptics),
        ));
        if archived {
            // Least invasive first: no instructions are sent, so the agent
            // waits for you instead of starting work.
            entries.push((
                "💬  Resume conversation (no prompt)".into(),
                TaskAction::Start(LaunchKind::Conversation {
                    session_id: String::new(),
                }),
            ));
            entries.push((
                "↺  Resume for revision (prior context)".into(),
                TaskAction::Start(LaunchKind::Revision {
                    session_id: String::new(),
                }),
            ));
            entries.push((
                "↺  Resume for revision + add context…".into(),
                TaskAction::Context { revision: true },
            ));
        }
        entries.push((
            "🤖  Daemon run logs…".into(),
            TaskAction::Later("Daemon run logs land with the extras phase."),
        ));
        entries.push(("↔  Move to stage…".into(), TaskAction::MoveStage));
        entries.push(("🌐  Open in browser".into(), TaskAction::OpenInBrowser));
        entries.push(("🗂  Repo folders…".into(), TaskAction::RepoFolders));
        let branch = state.config.target_branch(&task.project_name);
        entries.push((
            match branch {
                Some(branch) => format!("🎯  MR target branch ({branch})…"),
                None => "🎯  MR target branch…".to_string(),
            },
            TaskAction::TargetBranch,
        ));
        entries.push(("✕  Cancel".into(), TaskAction::Cancel));

        let list = SelectList::new(
            entries
                .iter()
                .map(|(label, _)| Line::raw(label.clone()))
                .collect(),
        );
        TaskMenu {
            task: Box::new(task.clone()),
            entries,
            list,
        }
    }

    /// The action a row performs, for tests and for the dispatcher.
    pub fn action(&self, index: usize) -> Option<&TaskAction> {
        self.entries.get(index).map(|(_, action)| action)
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.action(index).cloned() {
                None | Some(TaskAction::Cancel) => DialogOutcome::Close,
                Some(TaskAction::Later(message)) => DialogOutcome::Flash(message.to_string()),
                Some(TaskAction::Start(kind)) => {
                    DialogOutcome::Start(Box::new(StartRequest::new(&self.task, kind)))
                }
                Some(action) => DialogOutcome::Task(Box::new(TaskCommand {
                    task: self.task.clone(),
                    action,
                })),
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" #{} — {} ", self.task.id, truncate(&self.task.name, 44));
        let width = dialog_width(area, 64);
        let mut lines = self.list.lines(area.height);
        lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
        render_modal(frame, area, &title, color_from_name("cyan"), lines, width);
    }
}

/// Type something for the agent before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDialog {
    pub task: Box<Task>,
    pub revision: bool,
    pub prompt: TextPrompt,
}

impl ContextDialog {
    pub fn new(task: &Task, revision: bool) -> Self {
        ContextDialog {
            task: Box::new(task.clone()),
            revision,
            prompt: TextPrompt::new("", true),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.prompt.handle_key(key) {
            PromptOutcome::Stay => DialogOutcome::Stay,
            PromptOutcome::Cancel => DialogOutcome::Close,
            PromptOutcome::Submit(text) => {
                let kind = if self.revision {
                    LaunchKind::Revision {
                        session_id: String::new(),
                    }
                } else {
                    LaunchKind::Task
                };
                DialogOutcome::Start(Box::new(
                    StartRequest::new(&self.task, kind).context(text.trim()),
                ))
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = if self.revision {
            format!(" Resume #{} with extra context ", self.task.id)
        } else {
            format!(" Extra context for #{} ", self.task.id)
        };
        self.prompt.render(frame, area, &title);
    }
}
