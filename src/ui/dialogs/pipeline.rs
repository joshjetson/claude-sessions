//! The pipeline a project actually runs, as a flow you can read and edit.
//!
//! Ported from `PipelineView` in the Node app's `src/tui/dialogs.js`. The same
//! definitions drive the spawn prompt, so what is drawn here cannot go stale —
//! that is the whole reason the pipelines are data rather than template
//! literals inside the launch code.
//!
//! The footer is pinned below the scrolled region on purpose: "N of M steps
//! run" is the answer to "did my override take effect", so it must not be the
//! line that scrolls away.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::pipeline::project::project_pipeline_path;
use crate::pipeline::{resolve_pipeline, ResolvedPipeline, StepKind};
use crate::ui::dialogs::widgets::{dialog_width, gray, hint, render_modal};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;
use crate::util::truncate;

/// A project with no repo mapped has nowhere to put a `pipeline.json`. Say so
/// rather than letting the key appear to do nothing.
fn no_repo(project: &str) -> String {
    format!("No repo mapped for {project} — open the task menu and pick “Repo folders…”.")
}

/// Say it without closing: the flow is still what you were looking at.
fn keep_saying(message: String) -> DialogOutcome {
    DialogOutcome::Keep {
        action: None,
        flash: Some(message),
    }
}

pub struct PipelineView {
    pub project: String,
    pub repo: Option<String>,
    pub pipeline_id: String,
    pub resolved: Result<ResolvedPipeline, String>,
    pub sel: usize,
}

impl std::fmt::Debug for PipelineView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineView")
            .field("project", &self.project)
            .field("repo", &self.repo)
            .field("pipeline_id", &self.pipeline_id)
            .field("sel", &self.sel)
            .finish()
    }
}

impl Clone for PipelineView {
    fn clone(&self) -> Self {
        // `ResolvedPipeline` holds function pointers rather than closures, so a
        // clone is cheap and exact; re-resolving here would read the file again.
        PipelineView {
            project: self.project.clone(),
            repo: self.repo.clone(),
            pipeline_id: self.pipeline_id.clone(),
            resolved: self.resolved.clone(),
            sel: self.sel,
        }
    }
}

impl PipelineView {
    pub fn open(project: String, repo: Option<String>) -> Self {
        PipelineView::for_pipeline(project, repo, "task")
    }

    pub fn for_pipeline(project: String, repo: Option<String>, pipeline_id: &str) -> Self {
        let mut view = PipelineView {
            project,
            repo,
            pipeline_id: pipeline_id.to_string(),
            resolved: Err(String::new()),
            sel: 0,
        };
        view.reload();
        view
    }

    /// Re-read the override from disk, so the flow reflects whatever was just
    /// saved. A local JSON read, not a network call — this is the one lookup
    /// the dialog does synchronously.
    pub fn reload(&mut self) {
        self.resolved = resolve_pipeline(
            &self.pipeline_id,
            self.repo.as_ref().map(std::path::Path::new),
        )
        .map_err(|err| err.to_string());
    }

    fn steps(&self) -> &[crate::pipeline::ResolvedStep] {
        match &self.resolved {
            Ok(resolved) => &resolved.steps,
            Err(_) => &[],
        }
    }

    /// The file `e` opens and the line inside it the selected step sits on.
    fn edit_target(&self) -> Option<(String, u32)> {
        let repo = self.repo.as_ref()?;
        let file = project_pipeline_path(std::path::Path::new(repo));
        let line = self
            .steps()
            .get(self.sel)
            .map(|step| crate::pipeline::step_line_number(&file, &step.id))
            .unwrap_or(1);
        Some((file.to_string_lossy().into_owned(), line as u32))
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        let last = self.steps().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => DialogOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                DialogOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(last);
                DialogOutcome::Stay
            }
            KeyCode::Char('r') => {
                self.reload();
                DialogOutcome::Stay
            }
            // Always lands you in a file you can edit: the template is written
            // first when the project has none.
            KeyCode::Char('e') => match self.edit_target() {
                None => keep_saying(no_repo(&self.project)),
                Some((path, line)) => {
                    let repo = self.repo.clone().unwrap_or_default();
                    if !std::path::Path::new(&path).exists() {
                        let _ = crate::pipeline::init_project_pipeline(
                            std::path::Path::new(&repo),
                            &self.pipeline_id,
                        );
                    }
                    self.reload();
                    DialogOutcome::Keep {
                        action: Some(Action::OpenEditor { path, line }),
                        flash: None,
                    }
                }
            },
            KeyCode::Char('t') => match &self.repo {
                None => keep_saying(no_repo(&self.project)),
                Some(repo) => DialogOutcome::Keep {
                    action: Some(Action::WritePipelineTemplate {
                        repo: repo.clone(),
                        pipeline_id: self.pipeline_id.clone(),
                    }),
                    flash: None,
                },
            },
            _ => DialogOutcome::Stay,
        }
    }

    /// Every drawable line of the flow, selection marked.
    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let resolved = match &self.resolved {
            Err(error) => {
                return vec![Line::from(Span::styled(
                    error.clone(),
                    Style::default().fg(color_from_name("red")),
                ))]
            }
            Ok(resolved) => resolved,
        };
        let mut lines = vec![
            Line::from(Span::styled(resolved.summary.clone(), gray())),
            Line::default(),
        ];
        match (&resolved.override_error, &resolved.override_file) {
            (Some(error), _) => lines.push(Line::from(Span::styled(
                format!("override unreadable: {error}"),
                Style::default().fg(color_from_name("red")),
            ))),
            (None, Some(file)) => lines.push(Line::from(Span::styled(
                format!(
                    "▪ custom pipeline: {}",
                    short_path(file, self.repo.as_deref())
                ),
                Style::default().fg(color_from_name("cyan")),
            ))),
            (None, None) => lines.push(hint(
                "▪ default pipeline — press e to create and edit one for this project",
            )),
        }
        lines.push(Line::default());

        for (index, step) in resolved.steps.iter().enumerate() {
            let (dot, colour) = step_dot(step);
            let mut spans = vec![
                Span::styled(
                    format!(" {dot} "),
                    Style::default().fg(color_from_name(colour)),
                ),
                Span::styled(format!("{index:02}  "), gray()),
                Span::styled(
                    step.title.clone(),
                    if index == self.sel {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    },
                ),
            ];
            for (label, colour) in marks(step) {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    label.to_string(),
                    Style::default().fg(color_from_name(colour)),
                ));
            }
            lines.push(Line::from(spans));
            if let Some(skill) = &step.skill {
                lines.push(Line::from(Span::styled(
                    format!("   │    /{skill}"),
                    gray(),
                )));
            }
            if step.kind == StepKind::Dashboard {
                if let Some(stage) = &step.stage {
                    lines.push(Line::from(Span::styled(
                        format!("   │    → {stage}"),
                        gray(),
                    )));
                }
            }
            if index == self.sel {
                for text in [step.what.as_str(), step.detail.as_str()] {
                    for wrapped in wrap_plain(text, width.saturating_sub(8)) {
                        lines.push(Line::from(Span::styled(
                            format!("   │    {wrapped}"),
                            gray(),
                        )));
                    }
                }
            }
        }
        lines
    }

    /// The pinned footer: how much of the flow actually runs.
    pub fn footer(&self) -> Option<String> {
        let resolved = self.resolved.as_ref().ok()?;
        Some(format!(
            "{} of {} steps run",
            resolved.active_steps().len(),
            resolved.steps.len()
        ))
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 76);
        let mut lines = self.lines(width as usize);

        // Keep the selected step in view when the flow is taller than the frame.
        let budget = 6.max(area.height.saturating_sub(8) as usize);
        if lines.len() > budget {
            let anchor = self
                .lines_before_selection()
                .min(lines.len().saturating_sub(1));
            let start = anchor.saturating_sub(budget / 2).min(lines.len() - budget);
            lines = lines[start..start + budget].to_vec();
        }
        if let Some(footer) = self.footer() {
            lines.push(Line::from(Span::styled(footer, gray())));
        }
        lines.push(Line::default());
        lines.push(hint(
            "↑↓ step   e edit in $EDITOR   t write template   r reload   Esc close",
        ));
        render_modal(
            frame,
            area,
            &format!(" Pipeline · {} ", truncate(&self.project, 30)),
            color_from_name("cyan"),
            lines,
            width,
        );
    }

    /// Where the selected step's first line falls, for the scroll anchor.
    fn lines_before_selection(&self) -> usize {
        let all = self.lines(60);
        all.iter()
            .position(|line| {
                line.spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            })
            .unwrap_or(0)
    }
}

/// The dot in front of a step, by what it is.
fn step_dot(step: &crate::pipeline::ResolvedStep) -> (&'static str, &'static str) {
    if step.skipped {
        ("○", "gray")
    } else if step.gate {
        ("●", "yellow")
    } else if step.added {
        ("●", "green")
    } else if step.kind == StepKind::Dashboard {
        ("◆", "gray")
    } else {
        ("●", "cyan")
    }
}

fn marks(step: &crate::pipeline::ResolvedStep) -> Vec<(&'static str, &'static str)> {
    let mut marks = Vec::new();
    if step.kind == StepKind::Dashboard {
        marks.push(("dashboard", "gray"));
    }
    if step.gate {
        marks.push(("gate", "yellow"));
    }
    if step.added {
        marks.push(("added", "green"));
    } else if step.customised {
        marks.push(("custom", "cyan"));
    }
    if step.skipped {
        marks.push(("skipped", "gray"));
    }
    marks
}

/// The override path relative to the repo it lives in — the absolute one is
/// mostly the same prefix as everything else on screen.
fn short_path(file: &std::path::Path, repo: Option<&str>) -> String {
    let text = file.to_string_lossy();
    match repo {
        Some(repo) if text.starts_with(repo) => format!(".{}", &text[repo.len()..]),
        _ => text.into_owned(),
    }
}

/// Naive word wrap for the detail text — the pane is a fixed width.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() || width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}
