//! Where a task's transcript is.
//!
//! Three searches, and the NEWEST answer wins, because a task that goes back to
//! QA writes a new transcript each round and the last one is the one to resume:
//! one directory, that directory and its ancestors, or every project directory
//! at once. [`Archive::belongs_to_other_task`] lives here too — it asks the same
//! question of a transcript's head that the searches do, from the other end.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::db::TaskSession;
use crate::scan::session_files_in;
use crate::transcript::TaskRefCache;
use crate::types::SessionFile;
use crate::util::iso_now;

use super::Archive;

/// How far up from the directory an agent signed off in the folder search
/// walks. Four levels covers `repo/grails-app/assets/javascripts` and stops
/// well before anything shared.
const ANCESTOR_DEPTH: usize = 4;

/// How many transcripts the global search reads before it gives up. Newest
/// first, so a session from the last few days is found long before this.
pub const ANYWHERE_SCAN_LIMIT: usize = 400;

/// Head bytes read when asking a transcript which directory it ran in. The
/// first entry carries it; the rest of the file is irrelevant here.
const CWD_HEAD_BYTES: u64 = 64 * 1024;

/// A transcript found for a task, with the working directory it belongs to.
///
/// `cwd` is the directory the search was rooted at for the folder searches, and
/// the one the transcript itself reports for the global search — the project
/// directory name is a lossy encoding of a path and cannot be decoded back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSessionFile {
    pub file: SessionFile,
    pub cwd: String,
}

impl TaskSessionFile {
    /// The session id Claude Code resumes by: the file name without `.jsonl`.
    pub fn session_id(&self) -> String {
        session_id_of(&self.file.name)
    }

    pub fn path(&self) -> &Path {
        &self.file.path
    }
}

impl Archive<'_> {
    /// True when a transcript's opening prompt names a task other than this one.
    ///
    /// A transcript with NO task reference is not disqualified: sessions started
    /// by hand and merge-conflict sessions carry none, and they are legitimately
    /// archivable. Neither is an unreadable file — a transcript that vanished
    /// mid-check is not evidence of anything.
    pub fn belongs_to_other_task(
        &self,
        file: &Path,
        task_id: i64,
        refs: &mut TaskRefCache,
    ) -> bool {
        refs.get(file).is_some_and(|named| named != task_id)
    }

    /// The newest transcript in `cwd`'s project directory whose opening prompt
    /// names this task. Disambiguates sibling tasks worked in the same folder,
    /// which the process scanner can otherwise cross-link.
    pub fn find_task_session_file(
        &self,
        cwd: &str,
        task_id: i64,
        refs: &mut TaskRefCache,
    ) -> Option<TaskSessionFile> {
        if cwd.is_empty() {
            return None;
        }
        session_files_in(&self.paths.project_transcripts(cwd))
            .into_iter() // newest first
            .find(|file| refs.get(&file.path) == Some(task_id))
            .map(|file| TaskSessionFile {
                file,
                cwd: cwd.to_string(),
            })
    }

    /// The same search at `cwd` and then at its ancestors.
    ///
    /// An agent that ran `cd` into a subfolder before finishing reports a cwd no
    /// session directory matches — Claude keys its project directories by exact
    /// path, and one agent signed off from the repo's `grails-app/` subfolder,
    /// a level below where its transcript lived. The returned `cwd` is the
    /// directory the transcript was actually found under, not the one the caller
    /// handed in.
    pub fn find_task_session_near(
        &self,
        cwd: &str,
        task_id: i64,
        refs: &mut TaskRefCache,
    ) -> Option<TaskSessionFile> {
        if cwd.is_empty() {
            return None;
        }
        let mut dir = std::path::absolute(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
        for _ in 0..ANCESTOR_DEPTH {
            let here = dir.to_string_lossy().into_owned();
            if let Some(found) = self.find_task_session_file(&here, task_id, refs) {
                return Some(found);
            }
            let Some(parent) = dir.parent() else { break };
            // Never walk past the home directory: every task in every repo would
            // then share whatever transcripts were found there.
            if parent == dir || dir == self.paths.home {
                break;
            }
            dir = parent.to_path_buf();
        }
        None
    }

    /// A task's newest transcript, wherever it lives.
    ///
    /// The folder searches only look where the task was recorded. A QA round
    /// relocates the work to its own folder, which Claude keys as its own
    /// project directory and which is no parent of the repo, so a later round
    /// can be invisible to them — one task's archive stayed on round 1 for
    /// exactly that reason. This search ignores the folder.
    ///
    /// # The index, and the contract that keeps it honest
    ///
    /// The `task_session_index` row is consulted FIRST — one keyed SELECT and
    /// one `stat`, no directory reads and no transcript heads. Only a missing
    /// row, or a row whose file is gone, falls back to reading every project
    /// directory (Big-O mandate #4: that scan ran on every session that ended,
    /// stat-ing every `.jsonl` and reading a 128 KiB head from up to
    /// [`ANYWHERE_SCAN_LIMIT`] of them).
    ///
    /// The row is therefore the answer, which means whoever learns a task's
    /// session must record it: this module does so whenever it resolves one, and
    /// the daemon must do the same when it links a task to a live session. If a
    /// later round is never indexed while it runs, a stale row keeps pointing at
    /// the earlier one — the deviation from Node, which re-derived the answer
    /// from the filesystem every time and paid for it every time. Nothing unsafe
    /// follows from a stale row: what is archived or resumed is still checked
    /// against the transcript's own head.
    pub fn find_task_session_anywhere(
        &self,
        task_id: i64,
        refs: &mut TaskRefCache,
    ) -> Option<TaskSessionFile> {
        if let Some(indexed) = self.indexed_session(task_id) {
            return Some(indexed);
        }
        let Ok(dirs) = fs::read_dir(&self.paths.projects_dir) else {
            return None;
        };
        let mut files: Vec<SessionFile> = Vec::new();
        for dir in dirs.flatten() {
            files.extend(session_files_in(&dir.path()));
        }
        files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        let file = files
            .into_iter()
            .take(ANYWHERE_SCAN_LIMIT)
            .find(|file| refs.get(&file.path) == Some(task_id))?;
        let found = TaskSessionFile {
            cwd: session_cwd(&file.path).unwrap_or_default(),
            file,
        };
        self.remember_session(task_id, found.path(), &found.cwd);
        Some(found)
    }

    /// The indexed answer, or `None` when there is no row or its file is gone.
    fn indexed_session(&self, task_id: i64) -> Option<TaskSessionFile> {
        let row: TaskSession = self.db.task_session(task_id)?;
        if row.session_file.is_empty() {
            return None;
        }
        Some(TaskSessionFile {
            file: stat_session_file(Path::new(&row.session_file))?,
            cwd: row.cwd,
        })
    }

    /// Points the index at a transcript. Called wherever a resolution is
    /// committed, so the next lookup is one SELECT.
    pub(super) fn remember_session(&self, task_id: i64, session_file: &Path, cwd: &str) {
        self.db.put_task_session(&TaskSession {
            task_id,
            session_file: session_file.to_string_lossy().into_owned(),
            cwd: cwd.to_string(),
            updated_at: iso_now(),
        });
    }
}

/// The newest of several candidates, ties going to the earlier one — callers
/// list the folder searches before the global one, because a transcript found
/// where the task was recorded is the better answer when nothing is newer.
pub(super) fn newest_of<I>(candidates: I) -> Option<TaskSessionFile>
where
    I: IntoIterator<Item = Option<TaskSessionFile>>,
{
    let mut best: Option<TaskSessionFile> = None;
    for candidate in candidates.into_iter().flatten() {
        if best
            .as_ref()
            .is_none_or(|current| candidate.file.mtime > current.file.mtime)
        {
            best = Some(candidate);
        }
    }
    best
}

pub(super) fn session_id_of(file_name: &str) -> String {
    file_name
        .strip_suffix(".jsonl")
        .unwrap_or(file_name)
        .to_string()
}

pub(super) fn session_id_of_path(path: &Path) -> String {
    path.file_name()
        .map(|name| session_id_of(&name.to_string_lossy()))
        .unwrap_or_default()
}

/// A [`SessionFile`] for one known path, or `None` when it is gone — which is
/// how the index reports a row it can no longer honour.
fn stat_session_file(path: &Path) -> Option<SessionFile> {
    let meta = fs::metadata(path).ok()?;
    Some(SessionFile {
        name: path.file_name()?.to_string_lossy().into_owned(),
        path: path.to_path_buf(),
        mtime: meta.modified().ok()?,
        size: meta.len(),
        birthtime: meta.created().ok(),
    })
}

/// The working directory a transcript reports, read from the first entry in its
/// head that carries one. The project directory name is `/`-to-`-` encoded and
/// cannot be decoded back into a path, so the file itself is the only source.
fn session_cwd(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(CWD_HEAD_BYTES).read_to_end(&mut head).ok()?;
    // A head cut mid-line leaves one unparseable line at the end; every entry is
    // its own JSON object, so skipping it costs nothing.
    String::from_utf8_lossy(&head)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(
            |entry| match entry.get("cwd").and_then(|cwd| cwd.as_str()) {
                Some(cwd) if !cwd.is_empty() => Some(cwd.to_string()),
                _ => None,
            },
        )
}
