//! The Unix process table: `ps` and `lsof`.
//!
//! Split into its own file because it is the one part of discovery that a
//! platform either has or does not. The trait it implements, the row types and
//! every parser stay next door in `process.rs`, compiled and tested everywhere
//! — what is Unix-only is the four shell-outs, not the rules for reading what
//! they print.
//!
//! `ps -E` in particular is BSD-specific; see the platform notes in the
//! architecture brief.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::Duration;

use super::{parse_lsof_cwd, parse_pid_prefixed, parse_ps_listing, ProcessRow, ProcessSource};

/// How long any one of the four commands may take before its output is given
/// up on. Node passed the same 5s to `execFile`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Where macOS keeps the two commands discovery needs.
///
/// Resolved absolutely there because a daemon inherits the PATH of whatever
/// started it, and a dashboard launched from a GUI terminal, a login item or a
/// desktop launcher can be running with a PATH that has neither `/bin` nor
/// `/usr/sbin` on it — at which point every `ps` returns nothing and the
/// dashboard is empty with no error anywhere. Both paths are fixed on macOS.
///
/// Linux is left as bare names on purpose: distributions disagree about
/// `/bin` versus `/usr/bin` for `ps`, and `lsof` is a package that may not be
/// installed at all (which is why the `/proc` reader below exists), so PATH is
/// the more reliable answer there.
#[cfg(target_os = "macos")]
const PS_PATH: &str = "/bin/ps";
#[cfg(target_os = "macos")]
const LSOF_PATH: &str = "/usr/sbin/lsof";
#[cfg(not(target_os = "macos"))]
const PS_PATH: &str = "ps";
#[cfg(not(target_os = "macos"))]
const LSOF_PATH: &str = "lsof";

/// The absolute path if this machine has it, the bare name otherwise —
/// resolved once, because an absolute path either exists for the life of the
/// process or it never did, and a `stat` per call would be a syscall per
/// process per scan.
fn resolve(preferred: &'static str) -> &'static str {
    let bare = preferred.rsplit('/').next().unwrap_or(preferred);
    if preferred.starts_with('/') && !Path::new(preferred).exists() {
        return bare;
    }
    preferred
}

fn ps_program() -> &'static str {
    static PS: OnceLock<&'static str> = OnceLock::new();
    PS.get_or_init(|| resolve(PS_PATH))
}

fn lsof_program() -> &'static str {
    static LSOF: OnceLock<&'static str> = OnceLock::new();
    LSOF.get_or_init(|| resolve(LSOF_PATH))
}

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
        parse_ps_listing(&exec(
            ps_program(),
            &["-eo", "pid,tty,lstart,comm"],
            self.timeout,
        ))
    }

    fn cwds(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        // Where the kernel will simply tell us, ask it: a readlink is free
        // next to ~100ms of `lsof`, and `lsof` is a package a machine may
        // not have installed — without this, a Linux box without it shows an
        // empty dashboard.
        let pids = drain_from_proc(pids, &mut out, proc_cwd);
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
        self.read_or_ask(pids, "cmdline", &["-o", "pid=,command="])
    }

    fn environ(&self, pids: &[u32]) -> HashMap<u32, String> {
        // `ps -E` is BSD-specific: Linux has no equivalent flag, so the
        // fallback there produces nothing and `/proc/<pid>/environ` is the
        // only answer. Deliberately a different call from `argv`, so that no
        // environment value can be mistaken for a command-line flag.
        self.read_or_ask(pids, "environ", &["-ww", "-o", "pid=,command=", "-E"])
    }
}

impl SystemProcessSource {
    /// One `/proc` file per pid where the kernel exposes it, one batched `ps`
    /// for whatever is left.
    fn read_or_ask(&self, pids: &[u32], file: &str, args: &[&str]) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        let pids = drain_from_proc(pids, &mut out, |pid| proc_nul_separated(pid, file));
        if pids.is_empty() {
            return out;
        }
        let csv = pid_csv(&pids);
        let mut args = args.to_vec();
        args.extend(["-p", &csv]);
        out.extend(parse_pid_prefixed(&exec(ps_program(), &args, self.timeout)));
        out
    }
}

/// Answer what `/proc` can into `out`, and hand back the pids it could not.
fn drain_from_proc(
    pids: &[u32],
    out: &mut HashMap<u32, String>,
    read: impl Fn(u32) -> Option<String>,
) -> Vec<u32> {
    let mut remaining = Vec::new();
    for pid in pids {
        match read(*pid) {
            Some(value) => {
                out.insert(*pid, value);
            }
            None => remaining.push(*pid),
        }
    }
    remaining
}

/// `/proc/<pid>/cwd`, where there is one. `None` on every platform without
/// `/proc`, and for a process this user may not look at.
fn proc_cwd(pid: u32) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let target = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some(target.to_string_lossy().into_owned())
        .filter(|cwd| !cwd.is_empty() && !cwd.ends_with("(deleted)"))
}

/// `/proc/<pid>/cmdline` and `/proc/<pid>/environ`, which are NUL-separated
/// lists — joined with spaces so they parse by the same rules as the `ps`
/// output they stand in for.
fn proc_nul_separated(pid: u32, file: &str) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let raw = std::fs::read(format!("/proc/{pid}/{file}")).ok()?;
    let joined = raw
        .split(|byte| *byte == 0)
        .map(String::from_utf8_lossy)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(joined).filter(|joined| !joined.is_empty())
}

/// How many `lsof` calls are in flight at once. Bounded rather than unbounded
/// so a machine with a hundred sessions does not fork a hundred processes in
/// one go.
const MAX_CONCURRENT_LSOF: usize = 16;

fn lsof_cwd(pid: u32, timeout: Duration) -> Option<String> {
    let pid = pid.to_string();
    parse_lsof_cwd(&exec(
        lsof_program(),
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
        // `lstart` is read back by its month name and its four-digit year, so
        // the one thing that must not vary between machines is the locale
        // `ps` formats it in. Without this an exported LC_TIME can rewrite
        // every row into a shape the parser drops — which is an empty
        // dashboard, silently.
        .env("LC_ALL", "C")
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
