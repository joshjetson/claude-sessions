//! `claude-sessions iterm-script`: print the AppleScript, run nothing.
//!
//! Opus builds its iTerm2 scripts by calling a builder and handing the string
//! to `osascript` itself. These are the same builders the dashboard drives its
//! own tabs with, so a shim that asks here gets the dashboard's script rather
//! than a second copy that could drift. Nothing starts a process, so no
//! `SpawnPolicy` check applies.

use clap::ValueEnum;

/// The three builders Opus calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Which {
    /// Bring the tab on this tty device to the front
    Focus,
    /// Close the tab on this tty device
    Close,
    /// Open a tab running this shell line
    Viewer,
}

/// The script for `which`, exactly as the builder returns it.
pub fn script(which: Which, arg: &str) -> String {
    match which {
        Which::Focus => crate::term::build_focus_script(arg),
        Which::Close => crate::term::build_close_script(arg),
        Which::Viewer => crate::term::build_viewer_tab_script(arg),
    }
}
