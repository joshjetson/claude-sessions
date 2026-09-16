//! Ported from the Node app's `test/terminal.test.js` and `test/editor.test.js`.
//!
//! Nothing here starts a process. The argv vectors and AppleScript strings come
//! out of pure builders, so both drivers are covered on a machine with neither
//! tmux nor iTerm2 — and the spawn policy refuses inside a unit-test binary in
//! any case. The one suite that needs a real terminal is the gated integration
//! test in `tests/tmux_live.rs`.

mod editor;
mod iterm2;
mod select;
mod shell;
mod spawn;
mod tmux;
