//! Modal dialogs: the primitives, the dialogs built from them, and the one
//! exhaustive `match` that dispatches between them.
//!
//! The Node app kept all 28 dialogs plus their primitives in a single 1457-line
//! `dialogs.js` and dispatched on a string in a `switch` whose `default` was
//! silence — a typo'd dialog name simply did nothing. WORKING.md rule 4 splits
//! the file by area; the dispatcher below is an enum, so a dialog that exists
//! and is not handled will not compile.
//!
//! What is here is the sessions-view half: Phases 9b/10/11 add the board,
//! deploy, pipeline and log dialogs as further variants and further modules.

pub mod session;
pub mod settings;
pub mod shutdown;
pub mod viewer;
pub mod widgets;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::config::ConfigHandle;
use crate::ui::state::{Action, Quit};

pub use session::{AddGroup, KillConfirm, Rename, Search};
pub use settings::SettingsDialog;
pub use shutdown::ShutdownConfirm;
pub use viewer::{FileViewer, ViewerOutcome};
pub use widgets::{InlineChoice, ListOutcome, PromptOutcome, SelectList, TextPrompt};

/// What a dialog did with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Keep the dialog open.
    Stay,
    Close,
    /// Close, and hand this to the action queue.
    Act(Action),
    /// Close the dashboard.
    Quit(Quit),
}

/// What a dialog is allowed to reach while handling a key.
///
/// Deliberately narrow: config (dialogs save nicknames, groups and chat
/// settings) and nothing else. A dialog cannot spawn, signal or read the
/// session list — those arrive as constructor arguments or leave as actions.
pub struct DialogCtx<'a> {
    pub config: &'a mut ConfigHandle,
}

#[derive(Debug, Clone)]
pub enum Dialog {
    Kill(KillConfirm),
    Rename(Rename),
    AddGroup(AddGroup),
    Search(Search),
    Settings(SettingsDialog),
    Shutdown(ShutdownConfirm),
    FileViewer(FileViewer),
}

impl Dialog {
    /// A modal swallows input: the view beneath it never sees the key. That is
    /// what stops `q` quitting the dashboard from inside a confirmation.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        ctx: &mut DialogCtx<'_>,
    ) -> DialogOutcome {
        match self {
            Dialog::Kill(dialog) => dialog.handle_key(key, ctx),
            Dialog::Rename(dialog) => dialog.handle_key(key, ctx),
            Dialog::AddGroup(dialog) => dialog.handle_key(key, ctx),
            Dialog::Search(dialog) => dialog.handle_key(key, ctx),
            Dialog::Settings(dialog) => dialog.handle_key(key, ctx),
            Dialog::Shutdown(dialog) => dialog.handle_key(key),
            Dialog::FileViewer(dialog) => match dialog.handle_key(key, area) {
                ViewerOutcome::Stay => DialogOutcome::Stay,
                ViewerOutcome::Close => DialogOutcome::Close,
                ViewerOutcome::Open(path) => {
                    DialogOutcome::Act(Action::OpenEditor { path, line: 1 })
                }
            },
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, config: &ConfigHandle) {
        match self {
            Dialog::Kill(dialog) => dialog.render(frame, area),
            Dialog::Rename(dialog) => dialog.render(frame, area),
            Dialog::AddGroup(dialog) => dialog.render(frame, area),
            Dialog::Search(dialog) => dialog.render(frame, area),
            Dialog::Settings(dialog) => dialog.render(frame, area, config.chat()),
            Dialog::Shutdown(dialog) => dialog.render(frame, area),
            Dialog::FileViewer(dialog) => {
                // The only I/O in a render path, and it is a stat: the file is
                // re-read only when its mtime moved (brief §10 mandate #8).
                dialog.reload_if_changed();
                dialog.render(frame, area);
            }
        }
    }

    /// The name used in flashes and tests. Matches the Node dialog registry's
    /// string keys, so the smoke matrix reads the same in both codebases.
    pub fn name(&self) -> &'static str {
        match self {
            Dialog::Kill(_) => "killConfirm",
            Dialog::Rename(_) => "rename",
            Dialog::AddGroup(_) => "addGroup",
            Dialog::Search(_) => "search",
            Dialog::Settings(_) => "settings",
            Dialog::Shutdown(_) => "shutdown",
            Dialog::FileViewer(_) => "fileViewer",
        }
    }
}
