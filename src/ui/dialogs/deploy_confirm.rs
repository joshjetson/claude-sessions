//! The two Deploy-tab confirmations that are not merges: running the deploy
//! command, and handing a conflicted merge request to an agent.
//!
//! Ported from `DeployConfirm` / `ResolveConflictConfirm` in the Node app's
//! `src/tui/dialogs.js`.

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::deploy::{has_conflicts, has_shipped};
use crate::types::DeployTask;
use crate::ui::dialogs::widgets::{bold, coloured, render_confirm, InlineChoice, ListOutcome};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::state::{Action, AppState};
use crate::ui::theme::color_from_name;
use crate::util::truncate;

// --- deploy -------------------------------------------------------------------------

/// The highest-consequence action in the app. Everything it will do is on
/// screen before it runs: the literal command, the directory, the branch, what
/// will NOT ship, and how many tasks it will close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployConfirm {
    pub project: String,
    pub command: String,
    pub cwd: String,
    pub target_branch: String,
    pub outstanding: usize,
    pub unmerged: usize,
    pub shipped: usize,
    pub choice: InlineChoice,
}

impl DeployConfirm {
    pub fn build(project: &str, state: &AppState) -> Self {
        let settings = state.config.deploy_project_config(project);
        let entry = state.deploy.project(project);
        let tasks = entry.map(|entry| entry.tasks.as_slice()).unwrap_or(&[]);
        let shipped = tasks.iter().filter(|task| has_shipped(task)).count();
        let command = settings
            .as_ref()
            .map(|c| c.command.clone())
            .unwrap_or_default();
        let options: Vec<String> = if command.is_empty() {
            vec!["Close".to_string(), "Configure…".to_string()]
        } else {
            vec!["Cancel".to_string(), "Deploy".to_string()]
        };
        DeployConfirm {
            project: project.to_string(),
            cwd: settings
                .as_ref()
                .and_then(|c| c.cwd.as_ref())
                .map(|cwd| cwd.to_string_lossy().into_owned())
                .unwrap_or_else(|| "(the dashboard's own directory)".to_string()),
            target_branch: settings
                .as_ref()
                .map(|c| c.target_branch.clone())
                .unwrap_or_else(|| "main".to_string()),
            command,
            outstanding: tasks.len(),
            unmerged: tasks.len() - shipped,
            shipped,
            choice: InlineChoice::new(options),
        }
    }

    pub fn configured(&self) -> bool {
        !self.command.is_empty()
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            bold(format!("🚀 {}", truncate(&self.project, 46))),
            Line::default(),
        ];
        if !self.configured() {
            lines.push(coloured(
                "No deploy command configured for this project.",
                "red",
            ));
            lines.push(coloured("Press c on the project row to set one.", "gray"));
            return lines;
        }
        // The literal command, not a summary of it: this is the last chance to
        // notice it points at the wrong environment.
        lines.push(coloured(
            format!("$ {}", truncate(&self.command, 60)),
            "gray",
        ));
        lines.push(coloured(format!("in {}", truncate(&self.cwd, 60)), "gray"));
        lines.push(coloured(
            format!("ships from {}", truncate(&self.target_branch, 24)),
            "gray",
        ));
        lines.push(Line::default());
        if self.unmerged > 0 {
            let plural = if self.unmerged == 1 { "" } else { "s" };
            lines.push(coloured(
                format!(
                    "⚠ {} unmerged MR{plural} in the Deployed stage — they will NOT ship.",
                    self.unmerged
                ),
                "yellow",
            ));
        } else if self.outstanding == 0 {
            lines.push(coloured(
                "✓ Nothing outstanding in the Deployed stage.",
                "green",
            ));
        }
        if self.shipped > 0 {
            let plural = if self.shipped == 1 { "" } else { "s" };
            lines.push(coloured(
                format!(
                    "On success, {} task{plural} with merged MRs → Complete.",
                    self.shipped
                ),
                "green",
            ));
        }
        lines.push(Line::default());
        lines.push(coloured("This runs the production deploy now.", "yellow"));
        lines
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.choice.handle_key(key) {
            ListOutcome::Select(1) => {
                if self.configured() {
                    DialogOutcome::Act(Action::StartDeploy {
                        project: self.project.clone(),
                    })
                } else {
                    DialogOutcome::Configure(self.project.clone())
                }
            }
            ListOutcome::Select(_) | ListOutcome::Cancel => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border = if self.configured() { "yellow" } else { "red" };
        render_confirm(
            frame,
            area,
            " Deploy to production ",
            border,
            &self.lines(),
            &self.choice,
        );
    }
}

// --- resolve conflicts ----------------------------------------------------------------

/// Hand a conflicted merge request to an agent.
///
/// Spells out WHICH session it resumes and in WHICH folder, because "resume the
/// last session" silently starting a blank one in the wrong repository is the
/// failure mode worth preventing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveConflictConfirm {
    pub task: Box<DeployTask>,
    pub conflicted: bool,
    pub resumable: bool,
    pub session_id: Option<String>,
    pub dir: Option<String>,
    pub choice: InlineChoice,
}

impl ResolveConflictConfirm {
    pub fn build(task: &DeployTask, state: &AppState) -> Self {
        let link = state.board.link(task.id);
        let session_id = link
            .map(|link| link.session_id.clone())
            .filter(|id| !id.is_empty());
        let resumable = state.board.archived_tasks.contains(&task.id) || session_id.is_some();
        let dir = link
            .map(|link| link.cwd.clone())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| {
                state
                    .config
                    .odoo_project_dir_list(&task.project_name)
                    .first()
                    .cloned()
            });
        let conflicted = has_conflicts(task) && task.mr.is_some();
        let options: Vec<String> = if conflicted {
            vec![
                "Cancel".to_string(),
                if resumable {
                    "Resume & resolve".to_string()
                } else {
                    "Start & resolve".to_string()
                },
            ]
        } else {
            vec!["Close".to_string()]
        };
        ResolveConflictConfirm {
            task: Box::new(task.clone()),
            conflicted,
            resumable,
            session_id,
            dir,
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
        let Some(mr) = &self.task.mr else {
            lines.push(coloured("No merge request loaded for this task.", "red"));
            return lines;
        };
        if !self.conflicted {
            lines.push(coloured(
                format!("!{} does not report conflicts.", mr.iid),
                "yellow",
            ));
            lines.push(coloured(
                format!(
                    "state: {}{}",
                    mr.state,
                    if mr.merge_status.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", mr.merge_status.replace('_', " "))
                    }
                ),
                "gray",
            ));
            return lines;
        }

        lines.push(Line::from(vec![
            Span::styled(
                format!("!{}", mr.iid),
                Style::default().fg(color_from_name("cyan")),
            ),
            Span::styled(
                " has conflicts",
                Style::default().fg(color_from_name("red")),
            ),
        ]));
        lines.push(coloured(
            format!(
                "{} → {}",
                truncate(&mr.source_branch, 30),
                truncate(&mr.target_branch, 18)
            ),
            "gray",
        ));
        lines.push(Line::default());
        // Which session, by id. The agent that wrote the branch already knows
        // why every hunk looks the way it does; a fresh one does not.
        lines.push(match &self.session_id {
            Some(id) => coloured(
                format!("↺ Resumes the original session {}…", short(id)),
                "green",
            ),
            None if self.resumable => coloured("↺ Resumes this task's archived session.", "green"),
            None => coloured("No archived transcript — starts a fresh session.", "yellow"),
        });
        // And which folder. Resuming into the wrong repository is the failure
        // this line exists to prevent.
        lines.push(coloured(
            format!(
                "in {}",
                truncate(self.dir.as_deref().unwrap_or("(folder will be chosen)"), 52)
            ),
            "gray",
        ));
        lines.push(Line::default());
        lines.push(coloured(
            format!(
                "It merges {} in, reconciles each hunk,",
                truncate(&mr.target_branch, 16)
            ),
            "gray",
        ));
        lines.push(coloured(
            "pushes, and leaves the MR for you to merge.",
            "gray",
        ));
        lines
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.choice.handle_key(key) {
            ListOutcome::Select(1) if self.conflicted => {
                DialogOutcome::ResolveConflicts(self.task.clone())
            }
            ListOutcome::Select(_) | ListOutcome::Cancel => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border = if self.conflicted { "cyan" } else { "gray" };
        render_confirm(
            frame,
            area,
            " Resolve merge conflicts ",
            border,
            &self.lines(),
            &self.choice,
        );
    }
}

fn short(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}
