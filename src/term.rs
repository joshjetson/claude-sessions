//! Driving terminals: opening sessions, typing into live ones, focusing and
//! closing them, and handing the screen to an editor.
//!
//! The shape is deliberately layered so that almost none of it needs a process
//! to test:
//!
//! - `types` is the contract — [`TerminalDriver`], and the tty that joins a
//!   scanned session to a drivable pane.
//! - `tmux` and `iterm2` are *builders* first and drivers second: argv vectors
//!   and AppleScript strings come out of pure functions, and the driver below
//!   them only hands those to `exec`.
//! - `shell` holds the two things both drivers need, written once.
//! - `select` is the policy: which driver, given what exists on this machine.
//! - `spawn` is the gate everything goes through before a process starts.

mod editor;
mod exec;
mod iterm2;
mod select;
mod shell;
mod spawn;
mod tmux;
mod types;

pub use editor::{
    build_editor_argv, line_args, resolve_editor, resolve_editor_from_env, run_editor, Editor,
    EditorOutcome, EditorSource,
};
pub use exec::{
    open_args, CommandOutput, Exec, ExecRequest, Runner, MAX_OUTPUT, OPEN_COMMAND, OPEN_PREFIX,
    OPEN_TIMEOUT,
};
pub use iterm2::{
    build_close_script, build_focus_script, build_launch_script, build_send_text_script,
    build_viewer_tab_script, escape_applescript, escape_shell_single, Iterm2Driver,
};
pub use select::{
    choose_driver, driver_or_null, get_driver, make_driver, reset_driver_cache, DriverAvailability,
    DriverKind, Platform,
};
pub use shell::{build_shell_command, chunk_text, shell_quote, SEND_CHUNK_SIZE};
pub use spawn::{SpawnPolicy, SpawnRefused, NO_SPAWN_ENV};
pub use tmux::{
    build_attach_shell_command, build_kill_pane_args, build_new_session_args,
    build_new_window_args, build_send_enter_args, build_send_text_args,
    build_send_text_args_chunked, build_viewer_session_name, list_clients_args, list_panes_args,
    list_sessions_args, parse_client_list, parse_pane_list, parse_session_groups,
    pick_attached_group_session, Client, Pane, TmuxDriver, DETACHED_HEIGHT, DETACHED_WIDTH,
    LIST_CLIENTS_FORMAT, LIST_PANES_FORMAT, LIST_SESSIONS_FORMAT,
};
pub use types::{
    normalize_tty, DriverResult, LaunchRequest, NullDriver, SessionRef, TerminalDriver, RUN_ID_ENV,
    TASK_ID_ENV,
};

#[cfg(test)]
mod tests;
