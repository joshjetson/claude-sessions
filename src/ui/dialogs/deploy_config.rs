//! The Deploy tab's config editor: a project's command, directory and branch.
//!
//! Ported from `DeployConfig` in the Node app's `src/tui/dialogs.js`. Writes
//! straight through to `~/.claude-sessions.json` and refetches the board,
//! because the rows carry the command and the branch.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::config::DeployProjectPatch;
use crate::ui::dialogs::widgets::{
    dialog_width, hint, render_modal, ListOutcome, PromptOutcome, SelectList, TextPrompt,
};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::state::{Action, AppState};
use crate::ui::theme::color_from_name;
use crate::util::truncate;

// --- config ----------------------------------------------------------------------

/// Which field the editor is on. `None` is the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployField {
    Command,
    Cwd,
    TargetBranch,
}

impl DeployField {
    /// The note rides in the title because the prompt owns its own footer.
    /// "runs via sh -lc" is the one that earns its place: it explains why a
    /// profile-installed tool is on the PATH and why shell syntax works.
    fn title(self) -> &'static str {
        match self {
            DeployField::Command => " Deploy command — runs via sh -lc ",
            DeployField::Cwd => " Working directory — absolute path ",
            DeployField::TargetBranch => " Production branch — what production ships from ",
        }
    }

    fn patch(self, value: String) -> DeployProjectPatch {
        let mut patch = DeployProjectPatch::default();
        match self {
            DeployField::Command => patch.command = Some(value),
            DeployField::Cwd => patch.cwd = Some(value),
            DeployField::TargetBranch => patch.target_branch = Some(value),
        }
        patch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployConfig {
    pub project: String,
    pub command: String,
    pub cwd: String,
    pub target_branch: String,
    pub list: SelectList,
    /// The field being typed into, and its prompt.
    pub editing: Option<(DeployField, TextPrompt)>,
}

impl DeployConfig {
    pub fn new(project: &str, state: &AppState) -> Self {
        let settings = state.config.deploy_project_config(project);
        let mut dialog = DeployConfig {
            project: project.to_string(),
            command: settings
                .as_ref()
                .map(|c| c.command.clone())
                .unwrap_or_default(),
            cwd: settings
                .as_ref()
                .and_then(|c| c.cwd.as_ref())
                .map(|cwd| cwd.to_string_lossy().into_owned())
                .unwrap_or_default(),
            target_branch: settings
                .as_ref()
                .map(|c| c.target_branch.clone())
                .unwrap_or_else(|| "main".to_string()),
            list: SelectList::default(),
            editing: None,
        };
        dialog.rebuild();
        dialog
    }

    pub fn fields(&self) -> [(String, Option<DeployField>); 4] {
        [
            (
                format!(
                    "Command:   {}",
                    truncate(or(&self.command, "(not set)"), 44)
                ),
                Some(DeployField::Command),
            ),
            (
                format!(
                    "Directory: {}",
                    truncate(or(&self.cwd, "(project repo folder)"), 44)
                ),
                Some(DeployField::Cwd),
            ),
            (
                format!("Branch:    {}", truncate(&self.target_branch, 44)),
                Some(DeployField::TargetBranch),
            ),
            ("✕  Done".to_string(), None),
        ]
    }

    fn rebuild(&mut self) {
        let sel = self.list.sel;
        self.list = SelectList::new(
            self.fields()
                .into_iter()
                .map(|(label, _)| Line::raw(label))
                .collect(),
        );
        self.list.sel = sel.min(self.list.rows.len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        if let Some((field, prompt)) = &mut self.editing {
            let field = *field;
            return match prompt.handle_key(key) {
                PromptOutcome::Stay => DialogOutcome::Stay,
                PromptOutcome::Cancel => {
                    self.editing = None;
                    DialogOutcome::Stay
                }
                PromptOutcome::Submit(value) => {
                    let value = value.trim().to_string();
                    match field {
                        DeployField::Command => self.command.clone_from(&value),
                        DeployField::Cwd => self.cwd.clone_from(&value),
                        DeployField::TargetBranch => self.target_branch.clone_from(&value),
                    }
                    let _ = ctx
                        .config
                        .set_deploy_project_config(&self.project, field.patch(value));
                    self.editing = None;
                    self.rebuild();
                    // The board carries the command and branch on every row, so
                    // editing them means refetching it.
                    DialogOutcome::Keep {
                        action: Some(Action::RefreshDeploy),
                        flash: None,
                    }
                }
            };
        }

        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.fields().get(index).and_then(|(_, f)| *f) {
                None => DialogOutcome::Close,
                Some(field) => {
                    let initial = match field {
                        DeployField::Command => self.command.clone(),
                        DeployField::Cwd => self.cwd.clone(),
                        DeployField::TargetBranch => self.target_branch.clone(),
                    };
                    self.editing = Some((field, TextPrompt::new(initial, false)));
                    DialogOutcome::Stay
                }
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if let Some((field, prompt)) = &self.editing {
            return prompt.render(frame, area, field.title());
        }
        let title = format!(" ⚙ Deploy config — {} ", truncate(&self.project, 34));
        let mut lines = self.list.lines(area.height);
        lines.push(hint("Enter edit  ·  Esc close"));
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

/// `value`, or the placeholder when it is empty.
fn or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}
