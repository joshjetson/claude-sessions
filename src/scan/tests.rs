//! Ported from the Node app's `test/scanner.test.js`, which pins two production
//! incidents: sessions appearing and vanishing on their own (helper processes
//! counted as sessions) and "go to this session's terminal" opening the wrong
//! tab (rank pairing, then closest-pair getting eight launches wrong).
//!
//! Nothing here spawns a process. The operating system is reached through
//! [`super::ProcessSource`], and the pairing rules are pure, so the whole suite
//! runs in parallel against temp directories.

mod detect;
mod files;
mod launch_task;
mod pairing;
mod process;
mod projects;
mod scanner;
mod transcripts;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};

use crate::types::SessionFile;

use super::pairing::pair_processes_to_sessions;
use super::process::{ClaudeProcess, ProcessRow, ProcessSource};

/// The Node fixtures used bare millisecond numbers for both sides of the
/// comparison; the timings they encode are the measured ones, so they are kept
/// verbatim and turned into instants here.
pub(crate) fn at(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// A process with the Node helper's defaults: one tty, no declared session id,
/// no launch task. Tests override fields with struct-update syntax, which is
/// what `proc({ … })` did there.
pub(crate) fn a_proc(pid: u32, start_ms: u64) -> ClaudeProcess {
    ClaudeProcess {
        pid,
        tty: Some("ttys001".to_string()),
        lstart: "Wed Sep 16 14:10:37 2026".to_string(),
        start: Some(at(start_ms)),
        cwd: "/w".to_string(),
        session_id: None,
        launch_task_id: None,
        launch_run_id: None,
    }
}

/// A transcript, with the Node helper's defaults (mtime 2_000_000, born
/// 1_000_000). `birth_ms` of `None` is a filesystem that cannot say.
pub(crate) fn a_file(name: &str, mtime_ms: u64, birth_ms: Option<u64>) -> SessionFile {
    SessionFile {
        name: format!("{name}.jsonl"),
        path: Path::new("/p").join(format!("{name}.jsonl")),
        mtime: at(mtime_ms),
        size: 10,
        birthtime: birth_ms.map(at),
    }
}

/// What the caller passes: newest-started first, newest mtime first. Returns
/// `(pid, session id)` in the order the rules produced them.
pub(crate) fn pair(procs: &[ClaudeProcess], files: &[SessionFile]) -> Vec<(u32, String)> {
    pair_with(procs, files, &mut |_| None)
}

pub(crate) fn pair_with(
    procs: &[ClaudeProcess],
    files: &[SessionFile],
    task_ref: &mut dyn FnMut(&Path) -> Option<i64>,
) -> Vec<(u32, String)> {
    pair_processes_to_sessions(procs, files, task_ref)
        .into_iter()
        .map(|p| {
            let name = &files[p.file_index].name;
            (
                procs[p.proc_index].pid,
                name.strip_suffix(".jsonl").unwrap_or(name).to_string(),
            )
        })
        .collect()
}

/// A BSD `lstart` string for a fixed local afternoon plus `offset_secs`, so a
/// test can order processes without depending on when it runs.
pub(crate) fn lstart_at(offset_secs: i64) -> String {
    let base = Local
        .with_ymd_and_hms(2026, 9, 16, 14, 0, 0)
        .single()
        .expect("unambiguous local time");
    (base + chrono::Duration::seconds(offset_secs))
        .format("%a %b %e %H:%M:%S %Y")
        .to_string()
}

/// How many times each [`ProcessSource`] method was CALLED (not how many pids
/// it was asked about), so the caching claims can be asserted rather than
/// described.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Calls {
    pub(crate) list: u32,
    pub(crate) cwd: u32,
    pub(crate) argv: u32,
    pub(crate) environ: u32,
}

#[derive(Debug, Default)]
struct FakeState {
    rows: Vec<ProcessRow>,
    cwds: HashMap<u32, String>,
    argv: HashMap<u32, String>,
    environ: HashMap<u32, String>,
    calls: Calls,
}

/// A scripted process table. Cloning shares one table, so a test keeps a handle
/// on the same state the scanner is reading.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeProcesses {
    state: Rc<RefCell<FakeState>>,
}

impl FakeProcesses {
    pub(crate) fn new() -> Self {
        FakeProcesses::default()
    }

    /// Add a process. `argv` and `environ` default to the command name, which
    /// is what `ps` would print for a plain `claude`.
    pub(crate) fn add(&self, pid: u32, comm: &str, cwd: Option<&str>, start_secs: i64) -> &Self {
        let mut state = self.state.borrow_mut();
        state.rows.push(ProcessRow {
            pid,
            tty: Some(format!("ttys{pid:03}")),
            lstart: lstart_at(start_secs),
            comm: comm.to_string(),
        });
        if let Some(cwd) = cwd {
            state.cwds.insert(pid, cwd.to_string());
        }
        state.argv.insert(pid, comm.to_string());
        state.environ.insert(pid, comm.to_string());
        drop(state);
        self
    }

    pub(crate) fn with_argv(&self, pid: u32, argv: &str) -> &Self {
        self.state.borrow_mut().argv.insert(pid, argv.to_string());
        self
    }

    pub(crate) fn with_environ(&self, pid: u32, environ: &str) -> &Self {
        self.state
            .borrow_mut()
            .environ
            .insert(pid, environ.to_string());
        self
    }

    /// Keep the process in the listing but let the detail calls come back
    /// empty, as they do when a process exits between the two `ps` calls.
    pub(crate) fn forget_details(&self, pid: u32) -> &Self {
        let mut state = self.state.borrow_mut();
        state.argv.remove(&pid);
        state.environ.remove(&pid);
        drop(state);
        self
    }

    pub(crate) fn remove(&self, pid: u32) -> &Self {
        let mut state = self.state.borrow_mut();
        state.rows.retain(|row| row.pid != pid);
        state.cwds.remove(&pid);
        state.argv.remove(&pid);
        state.environ.remove(&pid);
        drop(state);
        self
    }

    pub(crate) fn calls(&self) -> Calls {
        self.state.borrow().calls
    }
}

impl ProcessSource for FakeProcesses {
    fn list(&self) -> Vec<ProcessRow> {
        let mut state = self.state.borrow_mut();
        state.calls.list += 1;
        state.rows.clone()
    }

    fn cwds(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut state = self.state.borrow_mut();
        state.calls.cwd += 1;
        pids.iter()
            .filter_map(|pid| state.cwds.get(pid).map(|v| (*pid, v.clone())))
            .collect()
    }

    fn argv(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut state = self.state.borrow_mut();
        state.calls.argv += 1;
        pids.iter()
            .filter_map(|pid| state.argv.get(pid).map(|v| (*pid, v.clone())))
            .collect()
    }

    fn environ(&self, pids: &[u32]) -> HashMap<u32, String> {
        let mut state = self.state.borrow_mut();
        state.calls.environ += 1;
        pids.iter()
            .filter_map(|pid| state.environ.get(pid).map(|v| (*pid, v.clone())))
            .collect()
    }
}

// --- reading the run id out of a process environment -------------------------

#[test]
fn a_run_id_is_read_from_the_environment() {
    let line = "PATH=/usr/bin CLAUDE_SESSIONS_RUN_ID=Project::Quality HOME=/Users/x";
    assert_eq!(
        crate::scan::detect::launch_run_id(line),
        Some("Project::Quality".to_string())
    );
}

#[test]
fn a_variable_that_merely_ends_in_the_key_is_not_a_run_id() {
    // The same boundary rule the task id uses. Without it, MY_CLAUDE_SESSIONS_RUN_ID
    // set by something else would be read as ours.
    let line = "MY_CLAUDE_SESSIONS_RUN_ID=nope";
    assert_eq!(crate::scan::detect::launch_run_id(line), None);
}

#[test]
fn an_environment_without_a_run_id_yields_none() {
    // Which is every session except a coordinator, so this is the common path.
    let line = "PATH=/usr/bin CLAUDE_SESSIONS_TASK_ID=6688 HOME=/Users/x";
    assert_eq!(crate::scan::detect::launch_run_id(line), None);
}

#[test]
fn an_empty_run_id_value_is_not_a_run_id() {
    let line = "CLAUDE_SESSIONS_RUN_ID= PATH=/usr/bin";
    assert_eq!(crate::scan::detect::launch_run_id(line), None);
}
