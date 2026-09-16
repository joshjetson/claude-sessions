//! Which local folder an Odoo project lives in, and which branch its merge
//! requests target.
//!
//! Ported from `AllDirsList` / `DirPicker` / `SavedDirPicker` / `FolderManager`
//! / `TargetBranch` in the Node app's `src/tui/dialogs.js`. The three pickers
//! share one list because they differ only in what they do with the answer —
//! Node had the same shape, and this keeps the "★ the guess goes first" rule in
//! one place.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::ui::board::StartRequest;
use crate::ui::dialogs::widgets::{
    dialog_width, hint, render_modal, ListOutcome, PromptOutcome, SelectList, TextPrompt,
};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::theme::color_from_name;

const CUSTOM: &str = "＋  Enter a custom path…";
const ADD_ANOTHER: &str = "＋  Add another folder…";

/// The folders to choose from, with the guess floated to the top and marked.
fn rows(dirs: &[String], guess: &str, last: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = dirs
        .iter()
        .map(|dir| {
            if dir == guess {
                Line::from(vec![
                    Span::styled(
                        "★ ",
                        ratatui::style::Style::default().fg(color_from_name("green")),
                    ),
                    Span::raw(dir.clone()),
                ])
            } else {
                Line::raw(format!("  {dir}"))
            }
        })
        .collect();
    lines.push(Line::from(Span::styled(
        last.to_string(),
        ratatui::style::Style::default().fg(color_from_name("cyan")),
    )));
    lines
}

/// The guess first, then everything else, with nothing listed twice.
fn ordered(dirs: Vec<String>, guess: &str, exclude: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(dirs.len() + 1);
    if !guess.is_empty() && !exclude.iter().any(|dir| dir == guess) {
        out.push(guess.to_string());
    }
    for dir in dirs {
        if exclude.contains(&dir) || out.contains(&dir) {
            continue;
        }
        out.push(dir);
    }
    out
}

/// Pick from every discovered folder. Used both to answer a launch and to add a
/// folder to a project's saved list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPicker {
    pub project: String,
    pub dirs: Vec<String>,
    pub guess: String,
    /// The launch waiting on an answer, when there is one.
    pub request: Option<Box<StartRequest>>,
    pub list: SelectList,
    /// Typing a path that no group contains.
    pub custom: Option<TextPrompt>,
}

impl DirPicker {
    pub fn for_launch(dirs: Vec<String>, guess: String, request: StartRequest) -> Self {
        let project = request.task.project_name.clone();
        DirPicker::new(project, dirs, guess, Some(Box::new(request)))
    }

    pub fn new(
        project: String,
        dirs: Vec<String>,
        guess: String,
        request: Option<Box<StartRequest>>,
    ) -> Self {
        let dirs = ordered(dirs, &guess, &[]);
        let list = SelectList::new(rows(&dirs, &guess, CUSTOM));
        DirPicker {
            project,
            dirs,
            guess,
            request,
            list,
            custom: None,
        }
    }

    fn chose(&self, dir: String) -> DialogOutcome {
        // The choice is remembered, so the next launch for this project needs
        // no prompt at all.
        DialogOutcome::PickDir(Box::new(super::PickedDir {
            project: self.project.clone(),
            dir,
            request: self.request.clone(),
        }))
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        if let Some(prompt) = self.custom.as_mut() {
            return match prompt.handle_key(key) {
                PromptOutcome::Stay => DialogOutcome::Stay,
                PromptOutcome::Cancel => {
                    self.custom = None;
                    DialogOutcome::Stay
                }
                PromptOutcome::Submit(path) => match path.trim() {
                    "" => DialogOutcome::Close,
                    path => self.chose(path.to_string()),
                },
            };
        }
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.dirs.get(index) {
                Some(dir) => self.chose(dir.clone()),
                None => {
                    self.custom = Some(TextPrompt::new("~/", false));
                    DialogOutcome::Stay
                }
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if let Some(prompt) = &self.custom {
            return prompt.render(frame, area, " Repo directory path ");
        }
        let title = format!(" Repo dir for \"{}\" ", self.project);
        let mut lines = self.list.lines(area.height);
        lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
        render_modal(
            frame,
            area,
            &title,
            color_from_name("cyan"),
            lines,
            dialog_width(area, 70),
        );
    }
}

/// A project with two or more saved folders: pick one, or add another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDirPicker {
    pub project: String,
    pub saved: Vec<String>,
    pub request: Box<StartRequest>,
    pub list: SelectList,
}

impl SavedDirPicker {
    pub fn new(saved: Vec<String>, request: StartRequest) -> Self {
        let project = request.task.project_name.clone();
        let list = SelectList::new(rows(&saved, "", ADD_ANOTHER));
        SavedDirPicker {
            project,
            saved,
            request: Box::new(request),
            list,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.list.handle_key(key) {
            ListOutcome::Stay | ListOutcome::Unhandled(_) => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => match self.saved.get(index) {
                Some(dir) => DialogOutcome::Start(Box::new(
                    self.request.as_ref().clone().in_dir(dir.clone()),
                )),
                // "Add another" widens the choice to every discovered folder,
                // carrying the waiting launch with it.
                None => DialogOutcome::AddFolder(self.request.clone()),
            },
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" Folder for \"{}\" ", self.project);
        let mut lines = self.list.lines(area.height);
        lines.push(hint("↑↓ move  ·  Enter select  ·  Esc cancel"));
        render_modal(
            frame,
            area,
            &title,
            color_from_name("cyan"),
            lines,
            dialog_width(area, 70),
        );
    }
}

/// Manage a project's saved folders. Pure management — it never launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderManager {
    pub project: String,
    pub saved: Vec<String>,
    pub discovered: Vec<String>,
    pub list: SelectList,
    /// Choosing a folder to add, from everything discovered.
    pub adding: Option<Box<DirPicker>>,
}

impl FolderManager {
    pub fn new(project: String, saved: Vec<String>, discovered: Vec<String>) -> Self {
        let list = SelectList::new(rows(&saved, "", "＋  Add a folder…"));
        FolderManager {
            project,
            saved,
            discovered,
            list,
            adding: None,
        }
    }

    fn reload(&mut self, ctx: &mut DialogCtx<'_>) {
        self.saved = ctx.config.odoo_project_dir_list(&self.project).to_vec();
        self.list = SelectList::new(rows(&self.saved, "", "＋  Add a folder…"));
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        if let Some(picker) = self.adding.as_mut() {
            return match picker.handle_key(key, ctx) {
                DialogOutcome::PickDir(picked) => {
                    let _ = ctx.config.add_odoo_project_dir(&self.project, &picked.dir);
                    self.adding = None;
                    self.reload(ctx);
                    DialogOutcome::Stay
                }
                DialogOutcome::Close => {
                    self.adding = None;
                    DialogOutcome::Stay
                }
                other => other,
            };
        }
        match self.list.handle_key(key) {
            ListOutcome::Stay => DialogOutcome::Stay,
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Select(index) => {
                if index >= self.saved.len() {
                    let dirs = ordered(self.discovered.clone(), "", &self.saved);
                    self.adding = Some(Box::new(DirPicker::new(
                        self.project.clone(),
                        dirs,
                        String::new(),
                        None,
                    )));
                }
                DialogOutcome::Stay
            }
            ListOutcome::Unhandled(key) => {
                let remove = matches!(key.code, KeyCode::Char('d') | KeyCode::Char('x'));
                if remove && self.list.sel < self.saved.len() {
                    let dir = self.saved[self.list.sel].clone();
                    let _ = ctx.config.remove_odoo_project_dir(&self.project, &dir);
                    self.reload(ctx);
                    self.list.sel = self.list.sel.min(self.saved.len());
                }
                DialogOutcome::Stay
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if let Some(picker) = &self.adding {
            return picker.render(frame, area);
        }
        let title = format!(" Repo folders for \"{}\" ", self.project);
        let mut lines = self.list.lines(area.height);
        lines.push(hint("Enter add  ·  d remove  ·  Esc close"));
        render_modal(
            frame,
            area,
            &title,
            color_from_name("cyan"),
            lines,
            dialog_width(area, 70),
        );
    }
}

/// A project's merge-request target branch. Empty clears it, which is how you
/// go back to the repository default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetBranch {
    pub project: String,
    pub prompt: TextPrompt,
}

impl TargetBranch {
    pub fn new(project: String, current: Option<&str>) -> Self {
        TargetBranch {
            project,
            prompt: TextPrompt::new(current.unwrap_or_default(), false),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.prompt.handle_key(key) {
            PromptOutcome::Stay => DialogOutcome::Stay,
            PromptOutcome::Cancel => DialogOutcome::Close,
            PromptOutcome::Submit(branch) => {
                let _ = ctx.config.set_target_branch(&self.project, branch.trim());
                DialogOutcome::Close
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" MR target branch for \"{}\" ", self.project);
        self.prompt.render(frame, area, &title);
    }
}
