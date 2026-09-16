//! One tick of discovery: processes in, grouped [`RawSession`]s out.
//!
//! [`Scanner`] owns every cache the scan needs and is meant to be **held across
//! ticks** — build one when the daemon starts and call
//! [`Scanner::scan_sessions`] on each poll. Dropping and rebuilding it between
//! ticks is correct but throws away the pid caches, which is most of what makes
//! a tick cheap (an `lsof` per process per second was ~70ms of every scan).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::paths::Paths;
use crate::transcript::TaskRefCache;
use crate::types::{RawSession, SessionStatus};
use crate::util::{cwd_to_project_dir, start_time_instant};

use super::detect::{
    is_daemon_scratch_cwd, is_helper_flag, is_interactive_claude, launch_task_id, session_id_flag,
};
use super::files::SessionFilesCache;
use super::pairing::pair_processes_to_sessions;
use super::process::{ClaudeProcess, ProcessRow, ProcessSource, SystemProcessSource};
use std::cmp::Reverse;

/// What one `ps -o command=` line said about a process. Fixed for its lifetime,
/// so it is read once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ArgvInfo {
    session_id: Option<String>,
    /// A helper in its flag spelling (`claude --bg-pty-host`), invisible to
    /// `ps -o comm`.
    helper: bool,
}

/// The scan, with its caches.
#[derive(Debug)]
pub struct Scanner<S: ProcessSource = SystemProcessSource> {
    source: S,
    paths: Paths,
    /// A process's working directory is fixed for its lifetime, so each pid is
    /// `lsof`ed once. All three pid caches are pruned to the live pids every
    /// tick, so none of them can grow unbounded.
    cwds: HashMap<u32, String>,
    argv: HashMap<u32, ArgvInfo>,
    launch_tasks: HashMap<u32, Option<i64>>,
    files: SessionFilesCache,
    task_refs: TaskRefCache,
}

impl Scanner<SystemProcessSource> {
    /// The scanner the daemon runs: real `ps`, real `lsof`.
    pub fn system(paths: Paths) -> Self {
        Scanner::new(SystemProcessSource::new(), paths)
    }
}

impl<S: ProcessSource> Scanner<S> {
    pub fn new(source: S, paths: Paths) -> Self {
        Scanner {
            source,
            paths,
            cwds: HashMap::new(),
            argv: HashMap::new(),
            launch_tasks: HashMap::new(),
            files: SessionFilesCache::new(),
            task_refs: TaskRefCache::new(),
        }
    }

    /// The transcript-head task ids read so far. Shared with the archive layer,
    /// which asks the same question of the same files.
    pub fn task_refs(&mut self) -> &mut TaskRefCache {
        &mut self.task_refs
    }

    /// The cached directory listings, for a caller that wants a project's
    /// transcripts without paying for a second readdir.
    pub fn session_files(&mut self) -> &mut SessionFilesCache {
        &mut self.files
    }

    /// Live `claude` processes with their cwd, declared session id and launch
    /// task resolved. Helpers, scratch-directory workers and processes whose
    /// cwd could not be read are already gone.
    pub fn processes(&mut self) -> Vec<ClaudeProcess> {
        let rows: Vec<_> = self
            .source
            .list()
            .into_iter()
            .filter(|row| is_interactive_claude(&row.comm))
            .collect();

        let alive: HashSet<u32> = rows.iter().map(|row| row.pid).collect();
        self.cwds.retain(|pid, _| alive.contains(pid));
        self.argv.retain(|pid, _| alive.contains(pid));
        self.launch_tasks.retain(|pid, _| alive.contains(pid));

        let need_cwd = uncached(&rows, &self.cwds);
        if !need_cwd.is_empty() {
            // A cwd that could not be read is NOT remembered: that process is
            // asked again next tick rather than being dropped for good.
            self.cwds.extend(self.source.cwds(&need_cwd));
        }

        let need_argv = uncached(&rows, &self.argv);
        if !need_argv.is_empty() {
            let lines = self.source.argv(&need_argv);
            for pid in &need_argv {
                let cmd = lines.get(pid).map(String::as_str).unwrap_or_default();
                // A process that exited between the two calls gets a definite
                // answer too, so it is not re-queried on every tick for as long
                // as it stays in the listing.
                self.argv.insert(
                    *pid,
                    ArgvInfo {
                        session_id: session_id_flag(cmd),
                        helper: is_helper_flag(cmd),
                    },
                );
            }
        }

        let need_env = uncached(&rows, &self.launch_tasks);
        if !need_env.is_empty() {
            let lines = self.source.environ(&need_env);
            for pid in &need_env {
                let line = lines.get(pid).map(String::as_str).unwrap_or_default();
                self.launch_tasks.insert(*pid, launch_task_id(line));
            }
        }

        rows.into_iter()
            .filter_map(|row| {
                let cwd = self.cwds.get(&row.pid)?.clone();
                let argv = self.argv.get(&row.pid).cloned().unwrap_or_default();
                if argv.helper || is_daemon_scratch_cwd(&cwd) {
                    return None;
                }
                Some(ClaudeProcess {
                    pid: row.pid,
                    tty: row.tty,
                    start: start_time_instant(&row.lstart),
                    lstart: row.lstart,
                    cwd,
                    session_id: argv.session_id,
                    launch_task_id: self.launch_tasks.get(&row.pid).copied().flatten(),
                })
            })
            .collect()
    }

    /// One tick: every live session, grouped by project directory.
    ///
    /// `now` is passed in rather than read, so a tick sees one instant and the
    /// placeholder rows below are testable without sleeping.
    pub fn scan_sessions(&mut self, now: SystemTime) -> Vec<RawSession> {
        let groups = group_by_project(self.processes());
        let mut sessions: Vec<RawSession> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

        for group in &groups {
            let dir = self.paths.project_transcripts(&group.cwd);
            seen_dirs.insert(dir.clone());
            let files = self.files.list(&dir);
            let task_refs = &mut self.task_refs;
            let pairs =
                pair_processes_to_sessions(&group.procs, files, &mut |path| task_refs.get(path));

            let mut paired: HashSet<u32> = HashSet::new();
            for pairing in &pairs {
                let proc = &group.procs[pairing.proc_index];
                let file = &files[pairing.file_index];
                paired.insert(proc.pid);
                let session_id = file
                    .name
                    .strip_suffix(".jsonl")
                    .unwrap_or(&file.name)
                    .to_string();
                if let Some(&index) = by_id.get(&session_id) {
                    sessions[index].pids.push(proc.pid);
                    continue;
                }
                // Fresh stat: the cached listing is only rebuilt when the
                // directory changes, and appending to a transcript does not
                // change that — but the status machine reads this mtime.
                let (mtime, size) = current_stat(&file.path).unwrap_or((file.mtime, file.size));
                by_id.insert(session_id.clone(), sessions.len());
                sessions.push(RawSession {
                    session_id,
                    pids: vec![proc.pid],
                    cwd: proc.cwd.clone(),
                    tty: proc.tty.clone(),
                    lstart: non_empty(&proc.lstart),
                    session_file: Some(file.path.clone()),
                    session_mtime: mtime,
                    session_size: Some(size),
                    status: None,
                    starting: false,
                });
            }

            // Live processes that have not written a transcript yet — freshly
            // spawned, or still at the trust prompt. Surfaced as `starting` so
            // a launch is visible immediately instead of as nothing at all;
            // they are never given a transcript, which is what stopped a
            // week-old session being shown as live on somebody else's tab.
            for proc in &group.procs {
                if paired.contains(&proc.pid) {
                    continue;
                }
                let session_id = format!("starting-{}", proc.pid);
                if let Some(&index) = by_id.get(&session_id) {
                    sessions[index].pids.push(proc.pid);
                    continue;
                }
                by_id.insert(session_id.clone(), sessions.len());
                sessions.push(RawSession {
                    session_id,
                    pids: vec![proc.pid],
                    cwd: proc.cwd.clone(),
                    tty: proc.tty.clone(),
                    lstart: non_empty(&proc.lstart),
                    session_file: None,
                    session_mtime: now,
                    session_size: None,
                    status: Some(SessionStatus::Starting),
                    starting: true,
                });
            }
        }

        self.files.prune(&seen_dirs);
        sessions
    }
}

/// The pids in `rows` that a cache has no answer for yet — the only ones any
/// of the three detail calls is made for.
fn uncached<T>(rows: &[ProcessRow], cache: &HashMap<u32, T>) -> Vec<u32> {
    rows.iter()
        .map(|row| row.pid)
        .filter(|pid| !cache.contains_key(pid))
        .collect()
}

/// The processes sharing one transcript directory, newest-started first.
struct ProjectGroup {
    /// The first cwd seen for this directory — several cwds can encode to one
    /// directory name, and Node kept the first too.
    cwd: String,
    procs: Vec<ClaudeProcess>,
}

fn group_by_project(procs: Vec<ClaudeProcess>) -> Vec<ProjectGroup> {
    let mut order: Vec<ProjectGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for proc in procs {
        let key = cwd_to_project_dir(&proc.cwd);
        match index.get(&key) {
            Some(&at) => order[at].procs.push(proc),
            None => {
                index.insert(key, order.len());
                order.push(ProjectGroup {
                    cwd: proc.cwd.clone(),
                    procs: vec![proc],
                });
            }
        }
    }
    for group in &mut order {
        group.procs.sort_by_key(|proc| Reverse(proc.start));
    }
    order
}

fn current_stat(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}
