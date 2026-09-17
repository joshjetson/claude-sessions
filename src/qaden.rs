//! QAden run-state reader.
//!
//! Ported from the Node app's `src/qaden.js`. QAden (the QA plugin) records
//! each QA pass as `run.json` in the task's QA directory. The file is the
//! authority on which round a task is in and which commit that round was
//! anchored to, so the board can label the QA menu entry with what will
//! actually happen instead of making the reviewer remember.
//!
//! Read-only, and deliberately forgiving: QAden is a separate tool on its own
//! release cycle, so a missing file, an unreadable one, or a schema we do not
//! recognise all resolve to "no prior run" rather than an error.
//!
//! Why the label matters: resuming a round anchored to a commit that no longer
//! exists re-reports FAILs the developer already fixed and hides regressions
//! the fix introduced. Starting a new round is the safe operation — QAden
//! archives the finished round to `run-round<N>.json` first — but it is still
//! not something to do by accident.
//!
//! One deviation from Node, and the reason this module has a cache: Node ran
//! `git rev-parse --short HEAD` synchronously *while formatting a menu row*, so
//! a slow repository froze the dashboard (brief §10 mandate #9). Here the file
//! reads are synchronous — they are small and local — and the git probe is
//! served from [`HeadCache`], which the action worker fills.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::paths::Paths;
use crate::term::Exec;

/// How long the `git rev-parse` probe may take. Node passed 4s.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(4);

/// What QAden knows about a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QaRunState {
    pub exists: bool,
    /// Archived rounds plus the live one; `0` when there has never been a run.
    pub round: u32,
    /// The commit the recorded round was anchored to.
    pub head: Option<String>,
    /// What the worktree is on now, when it is known.
    pub current_head: Option<String>,
    /// The two commits are both known and differ: the developer pushed a fix,
    /// so the next pass is a new round rather than a continuation.
    pub stale: bool,
    pub open_gaps: usize,
    pub closed_gaps: usize,
    pub dir: PathBuf,
    pub worktree: Option<PathBuf>,
}

/// `run.json` as QAden's `matrix.py` shapes it. Every field is optional: a
/// schema we do not recognise must read as "no prior run", never as an error.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RunFile {
    meta: RunMeta,
    cells: HashMap<String, Option<Cell>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RunMeta {
    head: Option<String>,
    worktree: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Cell {
    verdict: Option<serde_json::Value>,
}

/// Read a task's QA state.
///
/// `head_of` answers "what commit is this worktree on", and `None` means
/// "cannot tell" — see [`HeadCache`]. Nothing here spawns a process.
pub fn qa_run_state(
    paths: &Paths,
    task_id: i64,
    head_of: impl Fn(&Path) -> Option<String>,
) -> QaRunState {
    let dir = paths.qa_task_dir(task_id);
    let empty = QaRunState {
        dir: dir.clone(),
        ..QaRunState::default()
    };
    let Ok(raw) = fs::read_to_string(dir.join("run.json")) else {
        return empty;
    };
    let Ok(run) = serde_json::from_str::<RunFile>(&raw) else {
        return empty;
    };

    let cells = run.cells.values().flatten();
    let (mut open_gaps, mut closed_gaps) = (0, 0);
    for cell in cells {
        match &cell.verdict {
            Some(verdict) if !verdict.is_null() => closed_gaps += 1,
            _ => open_gaps += 1,
        }
    }

    // Archived rounds are run-round<N>.json beside the live file, so the round
    // in progress is one past however many have been archived.
    let archived = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| is_archived_round(&entry.file_name().to_string_lossy()))
                .count()
        })
        .unwrap_or(0);

    let head = run
        .meta
        .head
        .map(|head| head.trim().to_string())
        .filter(|head| !head.is_empty());
    let worktree = run.meta.worktree.map(PathBuf::from);
    let current_head = worktree.as_deref().and_then(&head_of);

    QaRunState {
        exists: true,
        round: archived as u32 + 1,
        // Only claim staleness when both commits are actually known. An absent
        // worktree means we cannot tell, and guessing "stale" would push the
        // reviewer toward archiving a round that may still be the current one.
        stale: match (&head, &current_head) {
            (Some(recorded), Some(current)) => recorded != current,
            _ => false,
        },
        head,
        current_head,
        open_gaps,
        closed_gaps,
        dir,
        worktree,
    }
}

/// `run-round<N>.json`, and nothing else in the directory.
fn is_archived_round(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("run-round") else {
        return false;
    };
    let Some(digits) = rest.strip_suffix(".json") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The QA menu label, which states what selecting it will do.
///
/// Deliberately not a bare "QA": the same key means start, continue, or open a
/// new round depending on state the reviewer cannot see from the board.
pub fn qa_menu_label(state: &QaRunState) -> String {
    if !state.exists {
        return "🧪  QA this task".to_string();
    }
    // Order matters. A round with every gap closed is finished, and offering to
    // "resume" it invites re-opening a completed pass — which is what the first
    // version of this function did against five real runs, each carrying 7-9
    // closed gaps and nothing left to do.
    if state.stale {
        return format!(
            "🧪  QA — start round {} (code changed since round {})",
            state.round + 1,
            state.round
        );
    }
    if state.closed_gaps > 0 && state.open_gaps == 0 {
        return format!(
            "🧪  QA — start round {} (round {} complete)",
            state.round + 1,
            state.round
        );
    }
    if state.open_gaps > 0 {
        let plural = if state.open_gaps == 1 { "" } else { "s" };
        return format!(
            "🧪  QA — resume round {}, {} gap{plural} open",
            state.round, state.open_gaps
        );
    }
    format!("🧪  QA — resume round {}", state.round)
}

/// The label for a task, read straight off disk with whatever heads are cached.
pub fn menu_label_for(paths: &Paths, task_id: i64, heads: &HeadCache) -> String {
    qa_menu_label(&qa_run_state(paths, task_id, |dir| heads.cached(dir)))
}

/// Short `HEAD` commits per worktree, cached by `(dir, mtime)`.
///
/// Two rules, both from brief §10: nothing in a render path spawns a process
/// (#9), and a viewer-style reader caches on mtime rather than re-running per
/// keystroke (#8). [`HeadCache::cached`] only ever answers from the map;
/// [`HeadCache::refresh`] is what runs `git`, and only the action worker calls
/// it.
#[derive(Debug, Default)]
pub struct HeadCache {
    entries: Mutex<HashMap<PathBuf, CachedHead>>,
}

/// The directory's mtime when the probe ran, and what it answered. `None` for
/// the head is a real answer — "not a git worktree, or it is gone" — and is
/// cached so a missing worktree is not re-probed on every frame.
type CachedHead = (Option<SystemTime>, Option<String>);

impl HeadCache {
    pub fn new() -> Self {
        HeadCache::default()
    }

    /// The cached head for a worktree, if one was read while the directory was
    /// in its current state. Never spawns; never blocks on anything but the
    /// map's own lock.
    pub fn cached(&self, dir: &Path) -> Option<String> {
        let mtime = dir_mtime(dir);
        let entries = self.entries.lock().ok()?;
        let (cached_at, head) = entries.get(dir)?;
        (*cached_at == mtime).then(|| head.clone()).flatten()
    }

    /// Run `git rev-parse --short HEAD` in `dir` and remember the answer.
    /// Returns what it stored.
    pub fn refresh(&self, exec: &Exec, dir: &Path) -> Option<String> {
        let head = head_of(exec, dir);
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(dir.to_path_buf(), (dir_mtime(dir), head.clone()));
        }
        head
    }

    /// Fill the cache for a task's recorded worktree, if it has one.
    pub fn refresh_for_task(&self, exec: &Exec, paths: &Paths, task_id: i64) {
        let state = qa_run_state(paths, task_id, |_| None);
        if let Some(worktree) = state.worktree {
            self.refresh(exec, &worktree);
        }
    }
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    fs::metadata(dir).ok().and_then(|meta| meta.modified().ok())
}

/// Short `HEAD` of a git working tree, or `None` if it is not one.
///
/// A worktree that is gone is not an error — it is the common case after a
/// worktree clean, and the caller must not read it as "stale".
fn head_of(exec: &Exec, dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let args = [
        "-C".to_string(),
        dir.display().to_string(),
        "rev-parse".to_string(),
        "--short".to_string(),
        "HEAD".to_string(),
    ];
    let output = exec.run_in("git", &args, None, HEAD_TIMEOUT);
    if !output.ok {
        return None;
    }
    let head = output.stdout.trim().to_string();
    (!head.is_empty()).then_some(head)
}

#[cfg(test)]
mod tests;
