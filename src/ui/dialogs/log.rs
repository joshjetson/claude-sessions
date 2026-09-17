//! The two log browsers: the daily standup log (`l`) and the auto-dev-daemon's
//! per-task run logs (`D`).
//!
//! Ported from `LogViewer` and `DaemonLogs` in the Node app's
//! `src/tui/dialogs.js`. Both read files, and neither re-reads them per
//! keystroke: the daily log caches the day it is showing and re-parses only
//! when the day changes or the file's mtime moves, and the run-log viewer is
//! [`FileViewer`], which already caches on `(path, mtime)` (brief §10 mandate
//! #8 — Node's versions did a `readdirSync` plus a full re-parse inside the
//! render function).

use std::path::PathBuf;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::autodev::{self, RunLog};
use crate::dailylog::{self, DayEntry};
use crate::db::Db;
use crate::paths::Paths;
use crate::ui::dialogs::widgets::{
    dialog_width, gray, hint, render_modal, ListOutcome, SelectList,
};
use crate::ui::dialogs::{DialogCtx, DialogOutcome, FileViewer, ViewerOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;

/// The daily log, one day at a time, `←` / `→` between days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogViewer {
    paths: Paths,
    /// Oldest → newest, the union of the markdown files and the database.
    days: Vec<String>,
    index: usize,
    scroll: usize,
    /// The day currently parsed, and the mtime it was parsed at.
    loaded: Option<(String, Option<SystemTime>)>,
    entries: Vec<DayEntry>,
    /// Counts re-reads so the cache can be asserted on rather than assumed.
    reads: u64,
}

impl LogViewer {
    /// Open on the newest day. Today's file is created first so there is always
    /// something to show and something for `o` to open.
    pub fn open(paths: &Paths, db: Option<&Db>, today: &str) -> Self {
        dailylog::ensure_daily_log(paths, today);
        let days = dailylog::list_log_dates(paths, db);
        let mut viewer = LogViewer {
            paths: paths.clone(),
            index: days.len().saturating_sub(1),
            days,
            scroll: 0,
            loaded: None,
            entries: Vec::new(),
            reads: 0,
        };
        viewer.reload_if_changed();
        viewer
    }

    pub fn day(&self) -> &str {
        self.days.get(self.index).map(String::as_str).unwrap_or("")
    }

    pub fn entries(&self) -> &[DayEntry] {
        &self.entries
    }

    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// Re-parse only when the day changed or its file did.
    fn reload_if_changed(&mut self) {
        let day = self.day().to_string();
        let mtime = std::fs::metadata(dailylog::daily_log_path(&self.paths, &day))
            .ok()
            .and_then(|meta| meta.modified().ok());
        if self.loaded.as_ref() == Some(&(day.clone(), mtime)) {
            return;
        }
        let raw = dailylog::read_log_for(&self.paths, &day);
        self.entries = dailylog::parse_day(&day, &raw);
        self.loaded = Some((day, mtime));
        self.reads += 1;
    }

    fn step_day(&mut self, delta: isize) {
        let last = self.days.len().saturating_sub(1);
        let next = (self.index as isize + delta).clamp(0, last as isize) as usize;
        if next != self.index {
            self.index = next;
            self.scroll = 0;
            self.reload_if_changed();
        }
    }

    /// Every rendered line for the current day, wrapped later by the widget.
    fn body(&self) -> Vec<Line<'static>> {
        if self.entries.is_empty() {
            return vec![hint("No completed tasks logged yet today.")];
        }
        let mut lines = Vec::new();
        for entry in &self.entries {
            let mut head = vec![
                Span::styled(
                    format!("#{}", entry.line.task_id),
                    Style::default().fg(color_from_name("green")),
                ),
                Span::raw("  "),
                Span::styled(
                    entry.line.title.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ];
            if !entry.line.time.is_empty() {
                head.push(Span::styled(format!("   {}", entry.line.time), gray()));
            }
            lines.push(Line::from(head));
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::raw(entry.line.short.clone()),
            ]));
            if let Some(url) = &entry.line.mr_url {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("↗ {}", mr_label(url)), gray()),
                ]));
            }
            lines.push(Line::default());
        }
        lines
    }

    fn view_height(area: Rect) -> usize {
        3.max(area.height.saturating_sub(6) as usize)
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        _ctx: &mut DialogCtx<'_>,
    ) -> DialogOutcome {
        self.reload_if_changed();
        let view_height = Self::view_height(area);
        let max = self.body().len().saturating_sub(view_height);
        let set = |value: isize, scroll: &mut usize| {
            *scroll = value.clamp(0, max as isize) as usize;
        };
        match key.code {
            // `l` closes it too: the key that opened it is the key that shuts
            // it, which is how every other toggle in the dashboard behaves.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') | KeyCode::Char('L') => {
                return DialogOutcome::Close
            }
            KeyCode::Char('o') => {
                let path = dailylog::openable_log(&self.paths, self.day());
                return DialogOutcome::Act(Action::OpenPath(path.display().to_string()));
            }
            KeyCode::Left | KeyCode::Char('[') => self.step_day(-1),
            KeyCode::Right | KeyCode::Char(']') => self.step_day(1),
            KeyCode::Up | KeyCode::Char('k') => set(self.scroll as isize - 1, &mut self.scroll),
            KeyCode::Down | KeyCode::Char('j') => set(self.scroll as isize + 1, &mut self.scroll),
            KeyCode::PageUp => set(
                self.scroll as isize - view_height as isize,
                &mut self.scroll,
            ),
            KeyCode::PageDown | KeyCode::Char(' ') => set(
                self.scroll as isize + view_height as isize,
                &mut self.scroll,
            ),
            KeyCode::Char('g') => self.scroll = 0,
            KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        DialogOutcome::Stay
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.reload_if_changed();
        let width = dialog_width(area, 100);
        let view_height = Self::view_height(area);
        let body = self.body();
        let max = body.len().saturating_sub(view_height);
        let top = self.scroll.min(max);

        let mut lines: Vec<Line<'static>> = body.into_iter().skip(top).take(view_height).collect();
        while lines.len() < view_height {
            lines.push(Line::default());
        }
        lines.push(Line::default());
        let nav = if self.days.len() > 1 {
            format!("  ({}/{})", self.index + 1, self.days.len())
        } else {
            String::new()
        };
        lines.push(hint(&format!(
            "↑↓ scroll  ·  ←→ day{nav}  ·  o open file  ·  Esc close"
        )));

        let percent = match (top * 100).checked_div(max) {
            Some(pct) => format!("  {pct}%"),
            None => String::new(),
        };
        let day = if self.day().is_empty() {
            "—"
        } else {
            self.day()
        };
        render_modal(
            frame,
            area,
            &format!(" Daily log · {day}{percent} "),
            color_from_name("cyan"),
            lines,
            width,
        );
    }
}

/// `…/-/merge_requests/12` → `MR !12`, anything else → `MR`.
fn mr_label(url: &str) -> String {
    match url.split("merge_requests/").nth(1) {
        Some(rest) => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() {
                "MR".to_string()
            } else {
                format!("MR !{digits}")
            }
        }
        None => "MR".to_string(),
    }
}

/// The auto-dev-daemon's run logs for a task: pick one, then read it.
///
/// No `PartialEq`: it owns a [`FileViewer`], which carries a file's contents.
#[derive(Debug, Clone)]
pub struct DaemonLogs {
    pub task_id: i64,
    /// The daemon state the task's tags say it is in, for the title.
    pub state: Option<&'static str>,
    logs: Vec<RunLog>,
    list: SelectList,
    viewing: Option<FileViewer>,
}

impl DaemonLogs {
    pub fn open(runs_dir: &std::path::Path, task_id: i64, tags: &[String]) -> Self {
        let logs = autodev::list_run_logs(runs_dir, task_id);
        let rows = logs
            .iter()
            .map(|log| {
                Line::from(vec![
                    Span::styled(
                        log.action.clone(),
                        Style::default().fg(color_from_name("cyan")),
                    ),
                    Span::raw("  "),
                    Span::styled(stamp(log), gray()),
                ])
            })
            .collect();
        DaemonLogs {
            task_id,
            state: autodev::auto_dev_state(tags).map(|state| state.label),
            logs,
            list: SelectList::new(rows),
            viewing: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.logs.is_empty()
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.logs.iter().map(|log| log.path.clone()).collect()
    }

    fn title(&self) -> String {
        match self.state {
            Some(state) => format!(" Daemon logs · #{} · {state} ", self.task_id),
            None => format!(" Daemon logs · #{} ", self.task_id),
        }
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        _ctx: &mut DialogCtx<'_>,
    ) -> DialogOutcome {
        if let Some(viewer) = self.viewing.as_mut() {
            return match viewer.handle_key(key, area) {
                // Back to the list rather than out of the dialog: you are
                // usually comparing two runs.
                ViewerOutcome::Close => {
                    self.viewing = None;
                    DialogOutcome::Stay
                }
                ViewerOutcome::Stay => DialogOutcome::Stay,
                ViewerOutcome::Open(path) => DialogOutcome::Act(Action::OpenPath(path)),
            };
        }
        if self.logs.is_empty() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => DialogOutcome::Close,
                _ => DialogOutcome::Stay,
            };
        }
        match self.list.handle_key(key) {
            ListOutcome::Select(index) => {
                if let Some(log) = self.logs.get(index) {
                    self.viewing = Some(FileViewer::open(format!(" {} ", log.name), &log.path));
                }
                DialogOutcome::Stay
            }
            ListOutcome::Cancel => DialogOutcome::Close,
            ListOutcome::Stay => DialogOutcome::Stay,
            ListOutcome::Unhandled(key) if key.code == KeyCode::Char('q') => DialogOutcome::Close,
            ListOutcome::Unhandled(_) => DialogOutcome::Stay,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(viewer) = self.viewing.as_mut() {
            viewer.reload_if_changed();
            viewer.render(frame, area);
            return;
        }
        let title = self.title();
        if self.logs.is_empty() {
            render_modal(
                frame,
                area,
                &title,
                color_from_name("cyan"),
                vec![
                    hint("No auto-dev-daemon run logs for this task."),
                    hint("(~/.local/share/auto-dev-daemon/runs/)"),
                ],
                dialog_width(area, 70),
            );
            return;
        }
        let mut lines = self.list.lines(area.height);
        lines.push(Line::default());
        lines.push(hint("Enter view  ·  Esc close"));
        render_modal(
            frame,
            area,
            &title,
            color_from_name("cyan"),
            lines,
            dialog_width(area, 78),
        );
    }
}

/// `Sep 1, 10:24 AM` — what Node's `toLocaleString` produced for a run log.
fn stamp(log: &RunLog) -> String {
    chrono::DateTime::<chrono::Local>::from(log.mtime)
        .format("%b %-d, %I:%M %p")
        .to_string()
}
