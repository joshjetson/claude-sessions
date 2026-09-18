//! Shared fixtures: a headless render, and state built on a temp directory.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};
use tempfile::TempDir;

use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::Paths;
use crate::types::{ConversationMessage, MessageRole, Session, SessionStatus, Usage};
use crate::ui::state::AppState;

/// Draw one frame into a `TestBackend` and hand back the buffer.
pub fn render<F: FnOnce(&mut Frame)>(width: u16, height: u16, body: F) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal.draw(body).expect("draw");
    terminal.backend().buffer().clone()
}

/// The painted text, one line per row — what a human would see.
pub fn text(buffer: &Buffer) -> String {
    let area = buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a dialog-shaped closure over a body-sized area.
pub fn render_area<F: FnOnce(&mut Frame, Rect)>(width: u16, height: u16, body: F) -> Buffer {
    render(width, height, |frame| {
        let area = frame.area();
        body(frame, area)
    })
}

/// A config handle on its own temporary home. The `TempDir` is returned so the
/// caller keeps it alive for the length of the test.
pub fn temp_config() -> (TempDir, ConfigHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    let config = ConfigHandle::load(&paths, EnvOverrides::default());
    (dir, config)
}

/// A state already on the sessions view. Most key tests want this: the default
/// view comes from config, and config's own default is the board.
pub fn sessions_state() -> (TempDir, AppState) {
    let (dir, mut state) = temp_state();
    state.view = crate::ui::state::View::Sessions;
    (dir, state)
}

pub fn temp_state() -> (TempDir, AppState) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    let config = ConfigHandle::load(&paths, EnvOverrides::default());
    (dir, AppState::new(paths, config))
}

/// A session with just enough filled in to render a row.
pub fn session(id: &str, cwd: &str, status: SessionStatus) -> Session {
    Session {
        session_id: id.to_string(),
        pids: vec![4242],
        cwd: cwd.to_string(),
        tty: Some("ttys004".to_string()),
        lstart: None,
        session_file: Some(PathBuf::from(format!("/transcripts/{id}.jsonl"))),
        session_mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        session_size: Some(1024),
        status,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
        run_id: None,
    }
}

pub fn usage(total: u64) -> Usage {
    Usage {
        input_tokens: Some(total),
        ..Usage::default()
    }
}

pub fn message(role: MessageRole, text: &str) -> ConversationMessage {
    ConversationMessage {
        role,
        text: text.to_string(),
        timestamp: "2026-09-16T14:05:06.000Z".to_string(),
        has_tool_use: false,
        usage: None,
    }
}
