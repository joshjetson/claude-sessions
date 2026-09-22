//! One tick of discovery: processes in, grouped [`RawSession`]s out.
//!
//! [`Scanner`] owns every cache the scan needs and is meant to be **held across
//! ticks** — build one when the daemon starts and call
//! [`Scanner::scan_sessions`] on each poll. Dropping and rebuilding it between
//! ticks is correct but throws away the pid caches, which is most of what makes
//! a tick cheap (an `lsof` per process per second was ~70ms of every scan).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::paths::Paths;
use crate::transcript::{SessionCwdCache, TaskRefCache};
use crate::types::{RawSession, SessionStatus};
use crate::util::{cwd_to_project_dir, start_time_instant};

use super::detect::{
    argv_is_interactive_claude, is_daemon_scratch_cwd, is_helper_flag, is_interactive_claude,
    is_script_runtime, launch_run_id, launch_task_id, session_id_flag,
};
use super::files::{current_stat, SessionFilesCache};
use super::pairing::pair_processes_to_sessions;
use super::process::{ClaudeProcess, PlatformProcessSource, ProcessRow, ProcessSource};
use super::transcripts::transcript_sessions;
use std::cmp::Reverse;

/// Where a tick's sessions come from.
///
/// Not a `cfg`: the whole transcript layer is compiled, tested and reasoned
/// about on every platform, and a test picks the variant it wants rather than
/// the compiler picking for it. [`Discovery::for_platform`] is what production
/// asks, and it asks [`crate::platform::LIVE_DISCOVERY`] — so the day native
/// process discovery lands on Windows, that flag flips and this layer switches
/// off with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// The process table names the sessions, and the transcripts are matched to
    /// the processes that are writing them. Every session carries a pid, a tty
    /// and a start time, and everything in the app that acts on a session
    /// works.
    Processes,
    /// The transcript store is the only evidence there is: a file written to
    /// recently is a session. No pid, no tty, no start time — see
    /// [`super::transcripts`] for what that costs and why it still beats an
    /// empty screen.
    Transcripts,
}

impl Discovery {
    pub const fn for_platform() -> Self {
        if crate::platform::LIVE_DISCOVERY {
            Discovery::Processes
        } else {
            Discovery::Transcripts
        }
    }
}

/// What one `ps -o command=` line said about a process. Fixed for its lifetime,
/// so it is read once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ArgvInfo {
    session_id: Option<String>,
    /// A helper in its flag spelling (`claude --bg-pty-host`), invisible to
    /// `ps -o comm`.
    helper: bool,
    /// The command line says this is an interactive session. Only consulted
    /// for a process whose `comm` is a [script
    /// runtime](is_script_runtime) — an npm or bun install of Claude Code,
    /// which `ps` reports as `node`.
    claude: bool,
}

impl ArgvInfo {
    /// Everything one command line is read for, in one place — the three
    /// questions used to be asked at three different points in the tick.
    fn read(argv: &str) -> Self {
        ArgvInfo {
            session_id: session_id_flag(argv),
            helper: is_helper_flag(argv),
            claude: argv_is_interactive_claude(argv),
        }
    }
}

/// The scan, with its caches.
#[derive(Debug)]
pub struct Scanner<S: ProcessSource = PlatformProcessSource> {
    source: S,
    paths: Paths,
    discovery: Discovery,
    /// A process's working directory is fixed for its lifetime, so each pid is
    /// `lsof`ed once. All three pid caches are pruned to the live pids every
    /// tick, so none of them can grow unbounded.
    cwds: HashMap<u32, String>,
    argv: HashMap<u32, ArgvInfo>,
    launch_tasks: HashMap<u32, Option<i64>>,
    /// The run a coordinator was launched for, cached per pid alongside it.
    launch_runs: HashMap<u32, Option<String>>,
    files: SessionFilesCache,
    task_refs: TaskRefCache,
    /// Where each transcript says it was started. Only [`Discovery::Transcripts`]
    /// asks — with a process there is an `lsof` answer, which is the process's
    /// own truth rather than what it wrote down.
    session_cwds: SessionCwdCache,
}

impl Scanner<PlatformProcessSource> {
    /// The scanner the daemon runs: real `ps` and real `lsof` where the
    /// platform has them, and a source that answers "no processes" where it
    /// does not. See [`crate::platform::LIVE_DISCOVERY`].
    pub fn system(paths: Paths) -> Self {
        Scanner::new(
            PlatformProcessSource::default(),
            paths,
            Discovery::for_platform(),
        )
    }
}

impl<S: ProcessSource> Scanner<S> {
    pub fn new(source: S, paths: Paths, discovery: Discovery) -> Self {
        Scanner {
            source,
            paths,
            discovery,
            cwds: HashMap::new(),
            argv: HashMap::new(),
            launch_tasks: HashMap::new(),
            launch_runs: HashMap::new(),
            files: SessionFilesCache::new(),
            task_refs: TaskRefCache::new(),
            session_cwds: SessionCwdCache::new(),
        }
    }

    /// The transcript-head task ids read so far. Shared with the archive layer,
    /// which asks the same question of the same files.
    pub fn task_refs(&mut self) -> &mut TaskRefCache {
        &mut self.task_refs
    }

    /// Where each transcript says it was started, for the layer that has no
    /// process to ask. Exposed for the same reason [`Scanner::task_refs`] is:
    /// one cache, asserted on rather than assumed.
    pub fn session_cwds(&mut self) -> &mut SessionCwdCache {
        &mut self.session_cwds
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
        let listing = self.source.list();

        // AN EMPTY LISTING IS A FAILED READ, NOT AN EMPTY MACHINE.
        //
        // `list()` cannot fail — `exec` returns an empty string for a spawn
        // failure or a timeout — so `ps` falling over is indistinguishable from
        // "no processes are running". Treated as truth it wiped every cache
        // below, which forced the next tick to re-read every cwd with `lsof`:
        // the slowest call there is, at the moment the machine is least able to
        // serve it.
        //
        // This process is itself in that listing, so a genuinely empty result
        // is impossible. Keep everything and let the next tick try again.
        if listing.is_empty() {
            return Vec::new();
        }

        let alive: HashSet<u32> = listing.iter().map(|row| row.pid).collect();
        self.cwds.retain(|pid, _| alive.contains(pid));
        self.argv.retain(|pid, _| alive.contains(pid));
        self.launch_tasks.retain(|pid, _| alive.contains(pid));
        self.launch_runs.retain(|pid, _| alive.contains(pid));

        // Two ways a row can be a session. Its own name settles it — a native
        // install, which is all the Node original ever handled — or its name is
        // only the script runtime executing it, and then nothing but the
        // command line can say. The second group is why a machine with Claude
        // Code installed from npm showed an empty dashboard.
        let considered: Vec<ProcessRow> = listing
            .into_iter()
            .filter(|row| is_interactive_claude(&row.comm) || is_script_runtime(&row.comm))
            .collect();

        // ONE batched `ps -o command=` for both groups, and once per pid ever —
        // a command line is fixed for a process's lifetime. Deliberately ahead
        // of the cwd read below: `lsof` costs ~100ms a process and must never
        // be paid for every `node` on the machine.
        let need_argv = uncached(&considered, &self.argv);
        if !need_argv.is_empty() {
            let lines = self.source.argv(&need_argv);
            for pid in &need_argv {
                let cmd = lines.get(pid).map(String::as_str).unwrap_or_default();
                // A process that exited between the two calls gets a definite
                // answer too, so it is not re-queried on every tick for as long
                // as it stays in the listing.
                self.argv.insert(*pid, ArgvInfo::read(cmd));
            }
        }

        // A runtime row has to be vouched for by its command line; a named row
        // is already in, and its argv only adds the session id and the
        // flag-spelled helper check further down.
        let rows: Vec<ProcessRow> = considered
            .into_iter()
            .filter(|row| {
                is_interactive_claude(&row.comm)
                    || self.argv.get(&row.pid).is_some_and(|argv| argv.claude)
            })
            .collect();

        let need_cwd = uncached(&rows, &self.cwds);
        if !need_cwd.is_empty() {
            // A cwd that could not be read is NOT remembered: that process is
            // asked again next tick rather than being dropped for good.
            self.cwds.extend(self.source.cwds(&need_cwd));
        }

        let need_env = uncached(&rows, &self.launch_tasks);
        if !need_env.is_empty() {
            let lines = self.source.environ(&need_env);
            for pid in &need_env {
                let line = lines.get(pid).map(String::as_str).unwrap_or_default();
                self.launch_tasks.insert(*pid, launch_task_id(line));
                // Same line, same call. A second `ps -E` per tick to read one
                // more variable would double the cost of the scan.
                self.launch_runs.insert(*pid, launch_run_id(line));
            }
        }

        rows.into_iter()
            .filter_map(|row| {
                // A process whose cwd could not be read is DROPPED — which is
                // right for one that has none, and wrong for one whose `lsof`
                // merely timed out. On a machine deep in swap those calls took
                // 1.6-4.1s against a 5s limit, so a slow moment silently
                // removed live sessions from the scan and the dashboard blanked.
                //
                // Failures are not cached (see above), so the retry happens on
                // the next tick; this only decides what to report meanwhile.
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
                    launch_run_id: self.launch_runs.get(&row.pid).cloned().flatten(),
                })
            })
            .collect()
    }

    /// One tick: every live session, however this platform can see them.
    ///
    /// `now` is passed in rather than read, so a tick sees one instant and the
    /// placeholder rows below are testable without sleeping.
    pub fn scan_sessions(&mut self, now: SystemTime) -> Vec<RawSession> {
        match self.discovery {
            Discovery::Processes => self.scan_processes(now),
            Discovery::Transcripts => transcript_sessions(
                &self.paths.projects_dir,
                &mut self.files,
                &mut self.session_cwds,
                now,
            ),
        }
    }

    /// One tick from the process table: every live session, grouped by project
    /// directory.
    fn scan_processes(&mut self, now: SystemTime) -> Vec<RawSession> {
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
                    run_id: proc
                        .launch_run_id
                        .as_deref()
                        .map(crate::qarun::decode_run_id),
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
                    run_id: proc
                        .launch_run_id
                        .as_deref()
                        .map(crate::qarun::decode_run_id),
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

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}
