//! Where the process list comes from.
//!
//! The four shell-outs the scanner needs sit behind [`ProcessSource`] so that
//! the rest of this module is testable without spawning anything, and so a
//! `/proc`-based Linux implementation can be dropped in later without a single
//! caller changing. The commands below are the macOS/BSD ones the Node app used
//! — `ps -E` in particular is BSD-specific (see the platform notes in the
//! architecture brief).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

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

/// How long any one of the four commands may take before its output is given
/// up on. Node passed the same 5s to `execFile`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// The real thing: `ps` and `lsof`.
#[derive(Debug, Clone)]
pub struct SystemProcessSource {
    timeout: Duration,
}

impl Default for SystemProcessSource {
    fn default() -> Self {
        SystemProcessSource {
            timeout: COMMAND_TIMEOUT,
        }
    }
}

impl SystemProcessSource {
    pub fn new() -> Self {
        SystemProcessSource::default()
    }
}

impl ProcessSource for SystemProcessSource {
    fn list(&self) -> Vec<ProcessRow> {
        parse_ps_listing(&exec("ps", &["-eo", "pid,tty,lstart,comm"], self.timeout))
    }

    fn cwds(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        for batch in pids.chunks(MAX_CONCURRENT_LSOF) {
            let answers: Vec<(u32, Option<String>)> = thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|pid| {
                        let (pid, timeout) = (*pid, self.timeout);
                        scope.spawn(move || (pid, lsof_cwd(pid, timeout)))
                    })
                    .collect();
                handles
                    .into_iter()
                    .filter_map(|handle| handle.join().ok())
                    .collect()
            });
            for (pid, cwd) in answers {
                if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
                    out.insert(pid, cwd);
                }
            }
        }
        out
    }

    fn argv(&self, pids: &[u32]) -> HashMap<u32, String> {
        if pids.is_empty() {
            return HashMap::new();
        }
        let csv = pid_csv(pids);
        parse_pid_prefixed(&exec(
            "ps",
            &["-o", "pid=,command=", "-p", &csv],
            self.timeout,
        ))
    }

    fn environ(&self, pids: &[u32]) -> HashMap<u32, String> {
        if pids.is_empty() {
            return HashMap::new();
        }
        let csv = pid_csv(pids);
        parse_pid_prefixed(&exec(
            "ps",
            &["-ww", "-o", "pid=,command=", "-E", "-p", &csv],
            self.timeout,
        ))
    }
}

/// How many `lsof` calls are in flight at once. Bounded rather than unbounded
/// so a machine with a hundred sessions does not fork a hundred processes in
/// one go.
const MAX_CONCURRENT_LSOF: usize = 16;

fn lsof_cwd(pid: u32, timeout: Duration) -> Option<String> {
    let pid = pid.to_string();
    parse_lsof_cwd(&exec(
        "lsof",
        &["-a", "-p", &pid, "-d", "cwd", "-Fn"],
        timeout,
    ))
}

fn pid_csv(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Run a command and return its stdout, or an empty string for anything that
/// went wrong — a missing binary, a non-zero exit, a timeout. Node's wrapper
/// resolved `''` on error too: a scan tick must never fail because `lsof`
/// did.
///
/// The wait happens on a helper thread so a hung command cannot stall a tick.
/// Reading the pipe on that same thread matters: a child whose output fills the
/// pipe buffer blocks until somebody drains it, and `ps -e` on a busy machine
/// is comfortably larger than a pipe.
fn exec(program: &str, args: &[&str], timeout: Duration) -> String {
    let Ok(child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return String::new();
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut child = child;
        let mut out = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut out);
        }
        let ok = matches!(child.wait(), Ok(status) if status.success());
        let _ = tx.send(if ok { out } else { String::new() });
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

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
