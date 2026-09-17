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
    /// The auto-dev daemon's run logs for this task — the same dialog `D`
    /// opens.
    DaemonLogs,
    OpenInBrowser,
    RepoFolders,
    TargetBranch,
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
        // cannot otherwise show. The `run.json` read is local and cheap; the
        // staleness probe is not, so it arrives later through `accept`.
        entries.push((
            crate::qaden::menu_label_for(&state.paths, task.id, &state.qa_heads),
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
        // Only when there is something to read, or a daemon tag saying there
        // will be — Node gated the row the same way (`dialogs.js:347`), and an
        // always-present row that opens an empty list is a row that teaches you
        // to skip it.
        if crate::autodev::has_run_logs(&state.paths.auto_dev_runs_dir, task.id)
            || crate::autodev::auto_dev_state(&task.tags).is_some()
        {
            entries.push(("🤖  Daemon run logs…".into(), TaskAction::DaemonLogs));
        }
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

    /// Swap in a QA label the worker resolved. Returns false when the answer
    /// was for another task — the cursor moved on while the probe ran.
    pub fn accept(&mut self, data: &crate::ui::dialogs::BoardData) -> bool {
        let crate::ui::dialogs::BoardData::QaLabel { task_id, label } = data else {
            return false;
        };
        if *task_id != self.task.id {
            return false;
        }
        let Some(index) = self
            .entries
            .iter()
            .position(|(_, action)| *action == TaskAction::Start(LaunchKind::Qa))
        else {
            return false;
        };
        self.entries[index].0 = label.clone();
        self.list.rows[index] = Line::raw(label.clone());
        true
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

// --- QA runs ----------------------------------------------------------------

/// What can be done with a QA run.
///
/// The labels say what each entry will NOT do as much as what it will. "Start
/// coordinator" sounds like it will answer things, and in shadow mode it does
/// not — so the label says so rather than leaving it to be discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAction {
    /// Start the QA sessions the run has lanes for.
    FillLanes,
    /// Start the coordinating session.
    StartCoordinator,
    /// Flip between shadow and triage. Applies to the NEXT coordinator, not one
    /// already running: its rules are in a prompt that has already been sent.
    ToggleMode,
    /// Stop watching. The QA sessions themselves keep running.
    StopWatching,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMenu {
    pub run_id: String,
    pub title: String,
    pub entries: Vec<(String, RunAction)>,
    pub list: SelectList,
}

impl RunMenu {
    pub fn build(run_id: &str, state: &AppState) -> Self {
        let run = state.board.runs.iter().find(|run| run.id == run_id);
        let (stage, covered, triage, lane_limit) = match run {
            Some(run) => (
                run.stage_name.clone(),
                run.task_ids.len(),
                run.mode == crate::qarun::RunMode::Triage,
                run.lane_limit.or_else(|| state.config.qa_lane_limit()),
            ),
            None => (String::new(), 0, false, None),
        };

        let cap = match lane_limit {
            Some(limit) => format!("up to {limit} at once"),
            None => "no cap".to_string(),
        };

        let agreement = crate::qarun::ShadowStore::new(&state.paths.runtime_dir)
            .agreement(run_id)
            .summary();

        let entries = vec![
            (
                format!(
                    "▶  Start QA sessions ({covered} task{}, {cap})",
                    if covered == 1 { "" } else { "s" }
                ),
                RunAction::FillLanes,
            ),
            (
                if triage {
                    "🧠  Start coordinator (triage — answers facts, escalates the rest)".to_string()
                } else {
                    "🧠  Start coordinator (shadow — records answers, gives none)".to_string()
                },
                RunAction::StartCoordinator,
            ),
            (
                if triage {
                    "↔  Switch to shadow mode (applies to the next coordinator)".to_string()
                } else {
                    "↔  Switch to triage mode (applies to the next coordinator)".to_string()
                },
                RunAction::ToggleMode,
            ),
            (format!("📊  Shadow agreement: {agreement}"), RunAction::Cancel),
            (
                "✕  Stop watching this run (sessions keep running)".to_string(),
                RunAction::StopWatching,
            ),
            ("✕  Cancel".to_string(), RunAction::Cancel),
        ];

        let list = SelectList::new(
            entries
                .iter()
                .map(|(label, _)| Line::raw(label.clone()))
                .collect(),
        );
        RunMenu {
            run_id: run_id.to_string(),
            title: stage,
            entries,
            list,
        }
    }

    fn action(&self, index: usize) -> Option<&RunAction> {
        self.entries.get(index).map(|(_, action)| action)
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.action(index).cloned() {
                None | Some(RunAction::Cancel) => DialogOutcome::Close,
                Some(action) => DialogOutcome::Run(Box::new(RunCommand {
                    run_id: self.run_id.clone(),
                    action,
                })),
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" QA run · {} ", truncate(&self.title, 40));
        let width = dialog_width(area, 72);
        let mut lines = self.list.lines(area.height);
        lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
        render_modal(frame, area, &title, color_from_name("cyan"), lines, width);
    }
}

/// A run action, addressed to the run it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCommand {
    pub run_id: String,
    pub action: RunAction,
}
