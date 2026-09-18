//! Where the process list comes from.
//!
//! The four shell-outs the scanner needs sit behind [`ProcessSource`] so that
//! the rest of this module is testable without spawning anything, and so a
//! `/proc`-based Linux implementation can be dropped in later without a single
//! caller changing. That seam is also what makes a platform with no readable
//! process table a supported platform rather than a broken one: it gets
//! [`UnsupportedProcessSource`], and everything above the trait is unchanged.
//!
//! The row types and every parser live here and are compiled and tested on
//! every platform. The `ps`/`lsof` implementation lives in `process/system.rs`
//! and exists only where those commands do.

use std::collections::HashMap;
use std::time::SystemTime;

#[cfg(unix)]
mod system;

#[cfg(unix)]
pub use system::SystemProcessSource;

/// One row of `ps -eo pid,tty,lstart,comm`, before anything is decided about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    /// `None` where `ps` printed `??` — no controlling terminal, so nothing for
    /// the terminal drivers to join on.
    pub tty: Option<String>,
    /// `lstart` verbatim (`Wed Sep 16 14:10:37 2026`). Kept as printed because
    /// the session row renders it through `util::format_start_time`.
    pub lstart: String,
    pub comm: String,
}

/// A live `claude` process, with everything the pairing rules need to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeProcess {
    pub pid: u32,
    pub tty: Option<String>,
    pub lstart: String,
    /// `lstart` as an instant. `None` when it could not be parsed — such a
    /// process can still be paired by identity, just not by timing.
    pub start: Option<SystemTime>,
    /// Kept as a string: it arrives from `lsof`, groups the sessions, and is
    /// encoded into a transcript directory name.
    pub cwd: String,
    /// The transcript named on the command line (`--resume` / `--session-id`).
    pub session_id: Option<String>,
    /// `CLAUDE_SESSIONS_RUN_ID` from the process environment. Set only on a QA
    /// run's coordinator, which is what makes it the way to recognise one.
    pub launch_run_id: Option<String>,
    /// `CLAUDE_SESSIONS_TASK_ID` from the process environment.
    pub launch_task_id: Option<i64>,
}

/// The operating-system facts the scanner reads, one method per shell-out.
///
/// Implementations return raw text where the Node app parsed raw text, so the
/// parsing rules live in one tested place instead of inside the command
/// wrapper.
pub trait ProcessSource {
    /// Every process on the machine: `ps -eo pid,tty,lstart,comm`.
    fn list(&self) -> Vec<ProcessRow>;

    /// Working directories: one `lsof -a -p <pid> -d cwd -Fn` per pid.
    ///
    /// Batched because it is answered CONCURRENTLY: `lsof` takes ~100ms a call,
    /// so running thirty of them one after another turns a first tick into
    /// three seconds. (Node fanned the same per-pid calls out through
    /// `Promise.all`.) Asked once per pid ever, never once per tick — a
    /// process's working directory is fixed for its lifetime.
    fn cwds(&self, pids: &[u32]) -> HashMap<u32, String>;

    /// Full command lines: `ps -o pid=,command= -p <csv>`.
    fn argv(&self, pids: &[u32]) -> HashMap<u32, String>;

    /// Command lines WITH the environment appended: `ps -ww -o pid=,command=
    /// -E -p <csv>`.
    ///
    /// Deliberately a different call from [`ProcessSource::argv`]: the two
    /// results are parsed by different rules, and mixing them would let an
    /// environment value be read as a command-line flag.
    fn environ(&self, pids: &[u32]) -> HashMap<u32, String>;
}

/// The process source for a platform whose process table this tool cannot read
/// yet.
///
/// Answers "no processes", which is emphatically not the same claim as "no
/// sessions": the transcripts are on disk and still being appended to, and
/// every view that reads them is unaffected. What is missing is the pairing of
/// a transcript to a live pid, and with it everything that needs one — the
/// live/idle status machine, the tty a driver would address, and killing.
///
/// Compiled on every platform, not just the ones that need it, so the
/// behaviour above the trait is exercised by the normal test run rather than
/// only in CI on the platform that degrades.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnsupportedProcessSource;

impl UnsupportedProcessSource {
    pub fn new() -> Self {
        UnsupportedProcessSource
    }
}

impl ProcessSource for UnsupportedProcessSource {
    fn list(&self) -> Vec<ProcessRow> {
        Vec::new()
    }

    fn cwds(&self, _pids: &[u32]) -> HashMap<u32, String> {
        HashMap::new()
    }

    fn argv(&self, _pids: &[u32]) -> HashMap<u32, String> {
        HashMap::new()
    }

    fn environ(&self, _pids: &[u32]) -> HashMap<u32, String> {
        HashMap::new()
    }
}

/// The source this platform actually uses.
///
/// A type alias rather than a second [`Scanner`](super::Scanner): every default
/// type parameter in the crate names this one type, so a platform swaps its
/// implementation without a single generic signature changing (WORKING.md
/// rule 5).
#[cfg(unix)]
pub type PlatformProcessSource = SystemProcessSource;
#[cfg(not(unix))]
pub type PlatformProcessSource = UnsupportedProcessSource;

/// `ps -eo pid,tty,lstart,comm` output into rows, header and junk skipped.
pub fn parse_ps_listing(out: &str) -> Vec<ProcessRow> {
    out.lines().filter_map(parse_ps_row).collect()
}

/// One listing row: `  9379 ??       Fri Aug 28 09:24:49 2026     /path/claude`.
///
/// Node matched `/^(\d+)\s+(\S+)\s+(.*\d{4})\s+(.*)$/`. Reproduced by fields
/// rather than by a greedy regex, because greedy `.*\d{4}` reaches into the
/// command whenever a command happens to contain four digits before a space.
/// The four-digit year ends `lstart`, and the original spacing is preserved so
/// the value renders exactly as `ps` printed it.
pub fn parse_ps_row(line: &str) -> Option<ProcessRow> {
    let line = line.trim();
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None; // the header row, and anything else that is not a process
    }
    let pid: u32 = line[..digits].parse().ok()?;
    let rest = line[digits..].trim_start();
    if rest.len() == line.len() - digits {
        return None; // no whitespace after the pid
    }
    let tty_len = rest.find(char::is_whitespace)?;
    let tty = &rest[..tty_len];
    let started = rest[tty_len..].trim_start();

    // Walk whitespace-separated tokens to the four-digit year.
    let mut year_end = None;
    let mut index = 0;
    for token in started.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        if trimmed.len() == 4 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
            year_end = Some(index + trimmed.len());
            break;
        }
        index += token.len();
    }
    let year_end = year_end?;
    let lstart = started[..year_end].to_string();
    let comm = started[year_end..].trim().to_string();

    Some(ProcessRow {
        pid,
        tty: normalise_tty(tty),
        lstart,
        comm,
    })
}

/// `??` is how `ps` spells "no controlling terminal".
fn normalise_tty(tty: &str) -> Option<String> {
    match tty {
        "" | "?" | "??" | "-" => None,
        other => Some(other.to_string()),
    }
}

/// `lsof -Fn` prints one field per line, each prefixed by its type; `n` is the
/// name, which for `-d cwd` is the working directory.
pub fn parse_lsof_cwd(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| line.strip_prefix('n').filter(|rest| !rest.is_empty()))
        .map(str::to_string)
}

/// `ps -o pid=,command=` output: a pid, then the rest of the line. Trimmed at
/// both ends, as Node's `line.trim().match(/^(\d+)\s+(.*)$/)` was.
pub fn parse_pid_prefixed(out: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for line in out.lines() {
        let line = line.trim();
        let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            continue;
        }
        let Ok(pid) = line[..digits].parse::<u32>() else {
            continue;
        };
        let rest = &line[digits..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        map.insert(pid, rest.trim_start().to_string());
    }
    map
}
