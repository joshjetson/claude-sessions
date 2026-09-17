//! Discovery: which processes are Claude Code sessions, which transcript each
//! one is writing, and what a project directory holds.
//!
//! The entry point is [`Scanner`], held across ticks for its caches. Everything
//! below it is small and testable on its own: [`is_interactive_claude`] and
//! friends decide what counts, [`pair_processes_to_sessions`] is pure, and the
//! operating system is reached only through [`ProcessSource`].
//!
//! There are two ways a session can be discovered, named by [`Discovery`]: from
//! the process table, which is what every Unix machine does, and from the
//! transcript store, which is what a machine whose process table this build
//! cannot read does instead. The second is a strictly smaller answer — no pid,
//! no tty — and never runs alongside the first.

mod detect;
mod files;
mod pairing;
mod process;
mod projects;
mod scanner;
mod transcripts;

pub use detect::{
    argv_is_interactive_claude, is_daemon_scratch_cwd, is_helper_flag, is_interactive_claude,
    is_script_runtime, launch_task_id, session_id_flag,
};
pub use files::{is_compacting, session_files_in, SessionFilesCache};
pub use pairing::{pair_processes_to_sessions, Pairing, BIRTH_SLACK_MS, BIRTH_WINDOW_MS};
#[cfg(unix)]
pub use process::SystemProcessSource;
pub use process::{
    parse_lsof_cwd, parse_pid_prefixed, parse_ps_listing, parse_ps_row, ClaudeProcess,
    PlatformProcessSource, ProcessRow, ProcessSource, UnsupportedProcessSource,
};
pub use projects::discover_projects;
pub use scanner::{Discovery, Scanner};
pub use transcripts::ACTIVITY_WINDOW;

#[cfg(test)]
mod tests;
