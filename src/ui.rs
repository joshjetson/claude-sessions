//! The dashboard: ratatui app shell, sessions view, conversation pane, dialogs.
//!
//! Ports the Node app's `ui.js`, `index.js`, `state.js`, `themes.js`,
//! `syntax.js` and the whole of `src/tui/`. The React-era scaffolding named in
//! brief §10 is deleted rather than ported: no blessed-tag round-trip
//! (`markup.js`), no store signature hashing, no frame coalescing — ratatui
//! renders straight from state and diffs the buffer itself.
//!
//! Layering, leaf-first:
//!
//! - [`spans`] and [`theme`] — display-width arithmetic and colour resolution.
//! - [`syntax`], [`conversation`], [`tree`] — state to styled lines.
//! - [`components`] — geometry and the three chrome pieces.
//! - [`state`] — one owned [`state::AppState`], no globals.
//! - [`keys`] — pure routing; blocking work leaves as a [`state::Action`].
//! - [`dialogs`] — the four primitives plus the sessions-view dialogs.
//! - [`feed`] — where sessions come from; Phase 6 plugs the daemon in here.
//! - [`board`] — the Odoo task board: rows, detail, the launch flow.
//! - [`actions`], [`run`] — the worker thread and the event loop.

pub mod actions;
pub mod app;
pub mod board;
pub mod components;
pub mod conversation;
pub mod dialogs;
pub mod feed;
pub mod keys;
pub mod run;
pub mod spans;
pub mod state;
pub mod syntax;
pub mod theme;
pub mod tree;

pub use run::run_dashboard;

#[cfg(test)]
mod tests;
