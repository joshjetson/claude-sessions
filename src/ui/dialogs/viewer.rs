//! A scrollable viewer for a file on disk, cached by `(path, mtime)`.
//!
//! Ported from `TextFileViewer` in the Node app's `src/tui/dialogs.js`, with its
//! one real defect fixed: Node called `readFileSync` inside the render function,
//! so holding `j` re-read the whole file once per keystroke. Brief §10 mandate
//! #8 says viewers cache by `(path, mtime)` — so this reads when the file is
//! opened, and again only when it has actually changed underneath.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal};
use crate::ui::theme::color_from_name;

/// Anything past this is tailed rather than read whole: a 40MB log should open
/// instantly at its end, which is the part anybody wants.
pub const TAIL_LIMIT: usize = 400_000;

#[derive(Debug, Clone)]
pub struct FileViewer {
    pub title: String,
    pub path: PathBuf,
    /// `None` means "stick to the bottom", which is where a log wants to open.
    pub scroll: Option<usize>,
    cached_mtime: Option<SystemTime>,
    lines: Vec<String>,
    reads: u64,
}

/// What a key did to the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerOutcome {
    Stay,
    Close,
    /// `o` hands the file to `$EDITOR` — which means suspending the dashboard,
    /// so the viewer asks rather than spawning anything itself.
    Open(String),
}

impl FileViewer {
    pub fn open(title: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let mut viewer = FileViewer {
            title: title.into(),
            path: path.into(),
            scroll: None,
            cached_mtime: None,
            lines: Vec::new(),
            reads: 0,
        };
        viewer.reload_if_changed();
        viewer
    }

    /// Re-read only when the file's mtime moved. Called once per draw; the stat
    /// is cheap, the read is not.
    pub fn reload_if_changed(&mut self) {
        let mtime = fs::metadata(&self.path)
            .ok()
            .and_then(|m| m.modified().ok());
        if mtime.is_some() && mtime == self.cached_mtime {
            return;
        }
        self.cached_mtime = mtime;
        self.lines = read_tail(&self.path);
        self.reads += 1;
    }

    /// How many times the file has actually been read. Exists so the cache can
    /// be asserted on rather than assumed — the same trick `TaskRefCache` uses.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn view_height(area: Rect) -> usize {
        3.max(area.height.saturating_sub(6) as usize)
    }

    fn max_scroll(&self, view_height: usize) -> usize {
        self.lines.len().saturating_sub(view_height)
    }

    pub fn handle_key(&mut self, key: KeyEvent, area: Rect) -> ViewerOutcome {
        let view_height = Self::view_height(area);
        let max = self.max_scroll(view_height);
        let current = self.scroll.unwrap_or(max);
        let mut set = |value: isize| {
            self.scroll = Some(value.clamp(0, max as isize) as usize);
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ViewerOutcome::Close,
            KeyCode::Char('o') => {
                return ViewerOutcome::Open(self.path.display().to_string());
            }
            KeyCode::Up | KeyCode::Char('k') => set(current as isize - 1),
            KeyCode::Down | KeyCode::Char('j') => set(current as isize + 1),
            KeyCode::PageUp => set(current as isize - view_height as isize),
            KeyCode::PageDown | KeyCode::Char(' ') => set(current as isize + view_height as isize),
            KeyCode::Char('g') => set(0),
            KeyCode::Char('G') => set(max as isize),
            _ => {}
        }
        ViewerOutcome::Stay
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let width = dialog_width(area, 120);
        let view_height = Self::view_height(area);
        let max = self.max_scroll(view_height);
        let top = self.scroll.unwrap_or(max).min(max);

        let mut lines: Vec<Line<'static>> = self
            .lines
            .iter()
            .skip(top)
            .take(view_height)
            .map(|l| Line::raw(l.clone()))
            .collect();
        while lines.len() < view_height {
            lines.push(Line::default());
        }
        lines.push(Line::default());
        lines.push(hint("↑↓ scroll  ·  o open file  ·  Esc back"));

        let percent = if max > 0 {
            format!("  {}%", top * 100 / max)
        } else {
            String::new()
        };
        render_modal(
            frame,
            area,
            &format!("{}{percent}", self.title),
            color_from_name("cyan"),
            lines,
            width,
        );
    }
}

/// The file's lines, tailed past [`TAIL_LIMIT`]. An unreadable file is a single
/// line saying so rather than an error: the viewer is already the error report.
fn read_tail(path: &Path) -> Vec<String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return vec!["(could not read file)".to_string()];
    };
    let mut body = if raw.len() > TAIL_LIMIT {
        // Cut on a character boundary, then drop the partial first line.
        let mut start = raw.len() - TAIL_LIMIT;
        while start < raw.len() && !raw.is_char_boundary(start) {
            start += 1;
        }
        format!("…(truncated)…\n{}", &raw[start..])
    } else {
        raw
    };
    while body.ends_with('\n') {
        body.pop();
    }
    body.split('\n').map(str::to_string).collect()
}
