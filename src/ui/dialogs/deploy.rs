//! The Deploy tab's two menus.
//!
//! Ported from `DeployMenu` / `DeployTaskMenu` in the Node app's
//! `src/tui/dialogs.js`. These are the discoverable forms of the shortcuts; the
//! config editor is in [`super::deploy_config`] and the confirmations in
//! [`super::merge`] and [`super::deploy_confirm`]. Neither menu merges or
//! deploys anything itself.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::deploy::{has_conflicts, merge_readiness};
use crate::types::DeployTask;
use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal, ListOutcome, SelectList};
use crate::ui::dialogs::{Dialog, DialogCtx, DialogOutcome};
use crate::ui::state::{Action, AppState};
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// What a deploy menu row does. Data rather than closures so a test can read
/// the menu without driving keys through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployAction {
    ViewOutput,
    CancelDeploy,
    MergeAll,
    Deploy,
    Configure,
    Refresh,
    Close,
}

// --- project menu -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployMenu {
    pub project: String,
    pub entries: Vec<(String, DeployAction)>,
    pub list: SelectList,
}

impl DeployMenu {
    pub fn build(project: &str, state: &AppState) -> Self {
        let running = state.deploy.is_running(project);
        let has_run = state.deploy.run(project).is_some();
        let configured = state
            .deploy
            .project(project)
            .is_some_and(|entry| !entry.command.is_empty());
        let ready = crate::ui::deploy::ready_targets(state, project).len();

        let mut entries: Vec<(String, DeployAction)> = Vec::new();
        if running {
            // A running deploy is the answer to "what is happening here", so
            // its output and its stop button come first.
            entries.push(("📜  View deploy output".into(), DeployAction::ViewOutput));
            entries.push((
                "✖  Cancel running deploy".into(),
                DeployAction::CancelDeploy,
            ));
        } else {
            if ready > 0 {
                entries.push((
                    format!("⇅  Merge all ready MRs ({ready})"),
                    DeployAction::MergeAll,
                ));
            }
            entries.push((
                if configured {
                    "🚀  Deploy to production".into()
                } else {
                    "🚀  Deploy (no command configured)".to_string()
                },
                DeployAction::Deploy,
            ));
            if has_run {
                entries.push((
                    "📜  View last deploy output".into(),
                    DeployAction::ViewOutput,
                ));
            }
        }
        entries.push((
            "⚙  Configure deploy command…".into(),
            DeployAction::Configure,
        ));
        entries.push(("↻  Refresh".into(), DeployAction::Refresh));
        entries.push(("✕  Cancel".into(), DeployAction::Close));

        DeployMenu {
            project: project.to_string(),
            list: rows(&entries),
            entries,
        }
    }

    pub fn action(&self, index: usize) -> Option<&DeployAction> {
        self.entries.get(index).map(|(_, action)| action)
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.action(index).cloned() {
                Some(action) => DialogOutcome::Deploy(Box::new(super::DeployCommand {
                    project: self.project.clone(),
                    action,
                })),
                None => DialogOutcome::Close,
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        menu(
            frame,
            area,
            &format!(" 🚀 {} ", truncate(&self.project, 44)),
            &self.list,
        );
    }
}

// --- task menu ------------------------------------------------------------------

/// What a task-menu row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTaskAction {
    /// Ready, so the confirmation opens. Blocked, so the reason is flashed —
    /// which is why the reason is IN the label as well.
    Merge,
    ResolveConflicts,
    GoToSession,
    OpenMr,
    OpenTask,
    MoveStage,
    DeployProject,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployTaskMenu {
    pub task: Box<DeployTask>,
    pub entries: Vec<(String, DeployTaskAction)>,
    pub list: SelectList,
}

impl DeployTaskMenu {
    pub fn build(task: &DeployTask, state: &AppState) -> Self {
        let readiness = merge_readiness(task);
        let mut entries: Vec<(String, DeployTaskAction)> = Vec::new();
        // The blocker reason is IN the label: a greyed-out "Merge" that only
        // explains itself once you press it is the thing this replaces.
        entries.push((
            match (&readiness.ready, &task.mr) {
                (true, Some(mr)) => format!("⇅  Merge !{}", mr.iid),
                _ => format!("⇅  Merge — blocked: {}", truncate(&readiness.reason, 34)),
            },
            DeployTaskAction::Merge,
        ));
        // Not for the QA role: resolving a conflict is development work.
        if has_conflicts(task) && state.role.shows_dev_actions() {
            // Conflicts are the one merge blocker an agent can clear, so the
            // offer sits right under Merge — and it says which session it will
            // use, because resuming the wrong one silently is the failure mode.
            let resumable = state.board.archived_tasks.contains(&task.id);
            entries.push((
                if resumable {
                    "🔀  Resolve conflicts (resume original session)".into()
                } else {
                    "🔀  Resolve conflicts (new session)".to_string()
                },
                DeployTaskAction::ResolveConflicts,
            ));
        }
        if crate::ui::board::task_session(state.sessions(), task.id, state.board.link(task.id))
            .is_some()
        {
            entries.push(("👁  View live session".into(), DeployTaskAction::GoToSession));
        }
        if !task.mr_url.is_empty() {
            entries.push(("🌐  Open merge request".into(), DeployTaskAction::OpenMr));
        }
        entries.push(("🌐  Open task in Odoo".into(), DeployTaskAction::OpenTask));
        entries.push(("↔  Move to stage…".into(), DeployTaskAction::MoveStage));
        entries.push((
            "🚀  Deploy this project…".into(),
            DeployTaskAction::DeployProject,
        ));
        entries.push(("✕  Cancel".into(), DeployTaskAction::Close));

        DeployTaskMenu {
            task: Box::new(task.clone()),
            list: rows(&entries),
            entries,
        }
    }

    pub fn action(&self, index: usize) -> Option<&DeployTaskAction> {
        self.entries.get(index).map(|(_, action)| action)
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.action(index).cloned() {
                None | Some(DeployTaskAction::Close) => DialogOutcome::Close,
                Some(action) => DialogOutcome::DeployTask(Box::new(super::DeployTaskCommand {
                    task: self.task.clone(),
                    action,
                })),
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" #{} — {} ", self.task.id, truncate(&self.task.name, 42));
        menu(frame, area, &title, &self.list);
    }
}

fn rows(entries: &[(String, impl Sized)]) -> SelectList {
    SelectList::new(
        entries
            .iter()
            .map(|(label, _)| Line::raw(label.clone()))
            .collect(),
    )
}

fn menu(frame: &mut Frame, area: Rect, title: &str, list: &SelectList) {
    let mut lines = list.lines(area.height);
    lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
    render_modal(
        frame,
        area,
        title,
        color_from_name("cyan"),
        lines,
        dialog_width(area, 64),
    );
}

/// Turn a menu choice into the dialog it opens or the action it enqueues.
///
/// Lives here rather than in the dispatcher so the menu's rows and what they do
/// are read together.
pub fn run_deploy_command(state: &mut AppState, project: &str, action: DeployAction) {
    match action {
        DeployAction::ViewOutput => crate::ui::deploy::show_run(state, project),
        DeployAction::CancelDeploy => {
            state.enqueue(Action::CancelDeploy {
                project: project.to_string(),
            });
            state.flash(format!("Sent SIGTERM to the {project} deploy."));
        }
        DeployAction::MergeAll => crate::ui::deploy::keys::open(
            state,
            Dialog::MergeAllConfirm(super::MergeAllConfirm::build(project, state)),
        ),
        DeployAction::Deploy => crate::ui::deploy::keys::open(
            state,
            Dialog::DeployConfirm(super::DeployConfirm::build(project, state)),
        ),
        DeployAction::Configure => crate::ui::deploy::keys::open(
            state,
            Dialog::DeployConfig(super::DeployConfig::new(project, state)),
        ),
        DeployAction::Refresh => crate::ui::deploy::keys::refresh(state),
        DeployAction::Close => {}
    }
}
