//! Which Odoo projects the board loads.
//!
//! Ported from `ProjectFilter` in the Node app's `src/tui/dialogs.js`. Space
//! cycles one project through the three states and Enter writes them all at
//! once, so a mis-press costs nothing until you commit.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::config::ConfigHandle;
use crate::odoo::OdooProject;
use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal, ListOutcome, SelectList};
use crate::ui::dialogs::{BoardData, DialogCtx, DialogOutcome};
use crate::ui::theme::color_from_name;

use super::odoo::Remote;

/// Which Odoo projects the board loads. Space cycles a project between the
/// three states; Enter writes them all at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPick {
    /// Neither listed: loaded in "mine", not in a scoped "all".
    Default,
    /// Loaded in "all" even when it is somebody else's.
    Include,
    /// Hidden in both views.
    Ignore,
}

impl ProjectPick {
    fn next(self) -> Self {
        match self {
            ProjectPick::Default => ProjectPick::Include,
            ProjectPick::Include => ProjectPick::Ignore,
            ProjectPick::Ignore => ProjectPick::Default,
        }
    }

    fn tag(self) -> (&'static str, &'static str) {
        match self {
            ProjectPick::Include => ("[load]  ", "green"),
            ProjectPick::Ignore => ("[ignore]", "red"),
            ProjectPick::Default => ("[ ·  ]  ", "gray"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFilter {
    pub projects: Remote<Vec<OdooProject>>,
    pub picks: BTreeMap<String, ProjectPick>,
    pub include: Vec<String>,
    pub ignore: Vec<String>,
    pub list: SelectList,
}

impl ProjectFilter {
    pub fn new(config: &ConfigHandle) -> Self {
        let filter = config.board_project_filter();
        ProjectFilter {
            projects: Remote::Loading,
            picks: BTreeMap::new(),
            include: filter.include,
            ignore: filter.ignore,
            list: SelectList::default(),
        }
    }

    pub fn accept(&mut self, data: &BoardData) -> bool {
        match data {
            BoardData::Projects(projects) => {
                self.picks = projects
                    .iter()
                    .map(|project| {
                        let pick = if self.include.contains(&project.name) {
                            ProjectPick::Include
                        } else if self.ignore.contains(&project.name) {
                            ProjectPick::Ignore
                        } else {
                            ProjectPick::Default
                        };
                        (project.name.clone(), pick)
                    })
                    .collect();
                self.projects = Remote::Ready(projects.clone());
                self.rebuild();
                true
            }
            BoardData::Failed {
                task_id: None,
                error,
            } => {
                self.projects = Remote::Failed(error.clone());
                true
            }
            _ => false,
        }
    }

    fn rebuild(&mut self) {
        let Remote::Ready(projects) = &self.projects else {
            return;
        };
        let sel = self.list.sel;
        self.list = SelectList::new(
            projects
                .iter()
                .map(|project| {
                    let pick = self
                        .picks
                        .get(&project.name)
                        .copied()
                        .unwrap_or(ProjectPick::Default);
                    let (tag, colour) = pick.tag();
                    Line::from(vec![
                        Span::styled(
                            tag.to_string(),
                            ratatui::style::Style::default().fg(color_from_name(colour)),
                        ),
                        Span::raw(format!("  {}", project.name)),
                    ])
                })
                .collect(),
        );
        self.list.sel = sel.min(self.list.rows.len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let Remote::Ready(projects) = self.projects.clone() else {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => DialogOutcome::Close,
                _ => DialogOutcome::Stay,
            };
        };
        match self.list.handle_key(key) {
            ListOutcome::Stay => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(_) => {
                let mut include = Vec::new();
                let mut ignore = Vec::new();
                for (name, pick) in &self.picks {
                    match pick {
                        ProjectPick::Include => include.push(name.clone()),
                        ProjectPick::Ignore => ignore.push(name.clone()),
                        ProjectPick::Default => {}
                    }
                }
                let _ = ctx
                    .config
                    .set_board_project_filter(Some(include), Some(ignore));
                // The board on screen answers the old filter, so it goes.
                DialogOutcome::FilterChanged
            }
            ListOutcome::Unhandled(key) => {
                if key.code == KeyCode::Char(' ') {
                    if let Some(project) = projects.get(self.list.sel) {
                        let pick = self
                            .picks
                            .entry(project.name.clone())
                            .or_insert(ProjectPick::Default);
                        *pick = pick.next();
                        self.rebuild();
                    }
                }
                DialogOutcome::Stay
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 70);
        let lines = match &self.projects {
            Remote::Loading => vec![hint("Loading projects…")],
            Remote::Failed(error) => vec![
                Line::from(Span::styled(
                    error.clone(),
                    ratatui::style::Style::default().fg(color_from_name("red")),
                )),
                hint("Esc to close"),
            ],
            Remote::Ready(_) => {
                let mut lines = self.list.lines(area.height);
                lines.push(hint("Space cycle  ·  Enter save  ·  Esc cancel"));
                lines
            }
        };
        render_modal(
            frame,
            area,
            " Board projects ",
            color_from_name("cyan"),
            lines,
            width,
        );
    }
}
