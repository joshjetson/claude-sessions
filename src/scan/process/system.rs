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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{parse_lsof_cwd, parse_pid_prefixed, parse_ps_listing, ProcessRow, ProcessSource};

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
