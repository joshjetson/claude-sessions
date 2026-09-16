//! Board tab tests, split by the Node suite each one ports.
//!
//! - [`linking`] — `test/task-session-link.test.js`
//! - [`gates`] — `test/dependencies.test.js` plus the duplicate-start guard
//! - [`revision`] — `test/revision-into-session.test.js` and
//!   `test/resume-conversation.test.js`
//! - [`launch`] — the launch flow at the worker, both sides of the spawn gate
//! - [`dialogs`] — the smoke matrix
//! - [`pickers`] — the folder pickers, the branch override, the flow viewer
//! - [`view`] — rows, cursor, detail pane and key map

mod dialogs;
mod fixtures;
mod gates;
mod launch;
mod linking;
mod pickers;
mod revision;
mod view;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::config::ConfigHandle;
use crate::ui::actions::BoardData;
use crate::ui::dialogs::Dialog;
use crate::ui::state::AppState;

/// The body rectangle the key handlers and dialogs size themselves against.
const BODY: Rect = Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 30,
};

pub(crate) fn press(state: &mut AppState, code: KeyCode) {
    crate::ui::keys::handle_key(state, KeyEvent::from(code), BODY);
}

pub(crate) fn press_ctrl(state: &mut AppState, ch: char) {
    crate::ui::keys::handle_key(
        state,
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
        BODY,
    );
}

/// Hand the open dialog an answer it was waiting on.
pub(crate) fn accept(state: &mut AppState, data: BoardData) {
    if let Some(dialog) = state.dialog.as_mut() {
        dialog.accept(&data);
    }
}

/// The open dialog, painted.
pub(crate) fn render_dialog(state: &mut AppState) -> String {
    let mut dialog = state.dialog.take().expect("a dialog to be open");
    let painted = render_one(&mut dialog, &state.config);
    state.dialog = Some(dialog);
    painted
}

pub(crate) fn render_one(dialog: &mut Dialog, config: &ConfigHandle) -> String {
    let buffer = crate::ui::tests::render(BODY.width, BODY.height, |frame| {
        let area = frame.area();
        dialog.render(frame, area, config);
    });
    crate::ui::tests::text(&buffer)
}
