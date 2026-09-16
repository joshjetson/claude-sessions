//! Keeping a finished task's conversation, so a later round can pick it up.
//!
//! When a task's session ends its transcript is copied to
//! `~/.claude-sessions/tasks/<id>/<sessionId>.jsonl`, with a `meta.json` beside
//! it and a row in the database, so `claude --resume <sessionId>` still works
//! weeks later — after Claude Code has rotated its own project directory, and
//! after the daemon that watched the session has been restarted.
//!
//! # No archive is better than a wrong one
//!
//! Every route into [`Archive::archive_task_conversation`] ends at the same
//! check: a transcript whose opening prompt names a DIFFERENT task is refused,
//! and the task is left with no archive at all. That is not defensiveness for
//! its own sake — one task was archived with another task's transcript, from a
//! different repo for a different client, and resuming it typed one task's
//! revision instructions into the other's agent. An unarchived task starts a
//! fresh session, which is merely unhelpful; a cross-linked one corrupts
//! somebody else's work. [`Archive::ensure_live_session`] applies the same
//! check from the other side, so a record that is already wrong is never handed
//! back.
//!
//! Finding the transcript in the first place is [`find`]; this file is the
//! record: what is stored, what is handed back, and what is refused.

use std::fs;
use std::path::{Path, PathBuf};

use crate::db::{Db, TaskArchive};
use crate::paths::Paths;
use crate::scan::session_files_in;
use crate::transcript::TaskRefCache;

mod find;

#[cfg(test)]
mod tests;

use find::{newest_of, session_id_of, session_id_of_path};
pub use find::{TaskSessionFile, ANYWHERE_SCAN_LIMIT};

/// What a caller knows about the session it wants archived. Everything is
/// optional: the searches in [`find`] are what actually locate the transcript,
/// and these are the hints they start from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveRequest {
    /// The directory the task was launched in, as recorded.
    pub cwd: String,
    /// A second place to look — the directory the agent actually signed off
    /// from. It is what saves the archive when the recorded link points at the
    /// wrong repo entirely.
    pub alt_cwd: String,
    pub session_id: String,
    /// The transcript a caller believes is the task's. Trusted only as far as
    /// [`Archive::belongs_to_other_task`] allows.
    pub session_file: Option<PathBuf>,
}

/// The archive, over one runtime tree and one database.
///
/// Cheap to construct — build one where it is needed rather than threading it
/// through. Every method that reads a transcript head takes the shared
/// [`TaskRefCache`], so a daemon pays for each file once per process life
/// whichever subsystem asked first.
#[derive(Debug)]
pub struct Archive<'a> {
    paths: &'a Paths,
    db: &'a Db,
}

impl<'a> Archive<'a> {
    pub fn new(paths: &'a Paths, db: &'a Db) -> Archive<'a> {
        Archive { paths, db }
    }

    fn meta_path(&self, task_id: i64) -> PathBuf {
        self.paths.task_dir(task_id).join("meta.json")
    }

    fn archived_copy(&self, task_id: i64, session_id: &str) -> PathBuf {
        self.paths
            .task_dir(task_id)
            .join(format!("{session_id}.jsonl"))
    }

    /// Whether this task has a conversation to resume. The database answers
    /// without touching the filesystem; `meta.json` remains the fallback, and
    /// covers archives written before the import.
    pub fn has_archive(&self, task_id: i64) -> bool {
        self.db.get_task_archive(task_id).is_some() || self.meta_path(task_id).exists()
    }

    /// The archive record, from the database or from `meta.json`.
    ///
    /// The two can disagree — a database that would not open leaves the file
    /// ahead, an import leaves the file behind. The row wins because it is what
    /// the board and the journal query, while the file is what survives a lost
    /// database. Neither is trusted about the transcript's CONTENT: that is
    /// re-checked against the bytes on disk before anything is resumed.
    pub fn task_meta(&self, task_id: i64) -> Option<TaskArchive> {
        if let Some(row) = self.db.get_task_archive(task_id) {
            return Some(row);
        }
        serde_json::from_str(&fs::read_to_string(self.meta_path(task_id)).ok()?).ok()
    }

    /// Path to the archived transcript for a task, if one was kept.
    pub fn archive_path(&self, task_id: i64) -> Option<PathBuf> {
        let meta = self.task_meta(task_id)?;
        if meta.session_id.is_empty() {
            return None;
        }
        let path = self.archived_copy(task_id, &meta.session_id);
        path.exists().then_some(path)
    }

    /// Every task id with an archive — the board's markers across restarts. One
    /// indexed query instead of a `stat` per task directory.
    pub fn list_archived_task_ids(&self) -> Vec<i64> {
        let rows = self.db.list_archived_task_ids();
        if !rows.is_empty() {
            return rows;
        }
        let Ok(entries) = fs::read_dir(&self.paths.tasks_dir) else {
            return Vec::new();
        };
        let mut ids: Vec<i64> = entries
            .flatten()
            .filter(|entry| entry.path().join("meta.json").exists())
            .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Copy this task's transcript into its archive and record it.
    ///
    /// Round 2 REPLACES round 1: a task that went back to QA has a newer
    /// conversation, and the archive is "the one to resume", not a history.
    /// Returns `None` when there is nothing safe to archive — see the module
    /// docs on why that is the good outcome.
    pub fn archive_task_conversation(
        &self,
        task_id: i64,
        request: &ArchiveRequest,
        refs: &mut TaskRefCache,
    ) -> Option<TaskArchive> {
        // Prefer the session whose transcript OPENS with this task — that is the
        // one truly spawned for it, whatever the caller was handed.
        let owned = newest_of([
            self.find_task_session_near(&request.cwd, task_id, refs),
            self.find_task_session_near(&request.alt_cwd, task_id, refs),
            self.find_task_session_anywhere(task_id, refs),
        ]);

        let (session_file, name, cwd) = match owned {
            Some(found) => {
                let cwd = if found.cwd.is_empty() {
                    request.cwd.clone()
                } else {
                    found.cwd
                };
                (found.file.path, found.file.name, cwd)
            }
            None => {
                let mut file = request.session_file.clone().filter(|path| path.exists());
                let mut name = request.session_id.clone();
                if file.is_none() && !request.cwd.is_empty() {
                    // Newest-first, but never a transcript that opens with a
                    // DIFFERENT task: that is another task's conversation which
                    // merely shares the folder.
                    let fallback = session_files_in(&self.paths.project_transcripts(&request.cwd))
                        .into_iter()
                        .find(|f| !self.belongs_to_other_task(&f.path, task_id, refs));
                    if let Some(found) = fallback {
                        name = found.name;
                        file = Some(found.path);
                    }
                }
                (file?, name, request.cwd.clone())
            }
        };
        if !session_file.exists() {
            return None;
        }

        // The last line of defence, and the one that matters: whatever route
        // chose this file — a caller-supplied link, the folder fallback — a
        // transcript whose spawn prompt names another task is not this task's
        // conversation, and archiving it is worse than archiving nothing.
        if self.belongs_to_other_task(&session_file, task_id, refs) {
            return None;
        }
        // `name` is a file name from a search and a bare id from a caller;
        // `session_id_of` accepts either, and the path is the last resort.
        let mut session_id = session_id_of(&name);
        if session_id.is_empty() {
            session_id = session_id_of_path(&session_file);
        }

        let dir = self.paths.task_dir(task_id);
        let _ = fs::create_dir_all(&dir);
        let _ = fs::copy(&session_file, dir.join(format!("{session_id}.jsonl")));

        let meta = TaskArchive {
            task_id,
            cwd,
            session_id,
            session_file: session_file.to_string_lossy().into_owned(),
            archived_at: crate::util::iso_now(),
        };
        self.write_meta(&meta);
        Some(meta)
    }

    /// `meta.json` stays the on-disk record so anything reading it keeps
    /// working; the rows are what the board, the journal and the next lookup
    /// actually query.
    fn write_meta(&self, meta: &TaskArchive) {
        if let Ok(json) = serde_json::to_string_pretty(meta) {
            let _ = fs::create_dir_all(self.paths.task_dir(meta.task_id));
            let _ = fs::write(self.meta_path(meta.task_id), json);
        }
        self.db.put_task_archive(meta);
        self.remember_session(meta.task_id, Path::new(&meta.session_file), &meta.cwd);
    }

    /// Make sure the live transcript exists so `claude --resume` can find it,
    /// restoring it from the archive when Claude's project directory was
    /// cleaned, and repairing a record a later round left behind.
    ///
    /// `None` means "nothing safe to resume": callers read that as "start a
    /// fresh session", which is the right answer both for a task that was never
    /// archived and for one whose stored record points at somebody else's
    /// conversation.
    pub fn ensure_live_session(
        &self,
        task_id: i64,
        refs: &mut TaskRefCache,
    ) -> Option<TaskArchive> {
        let mut meta = self.task_meta(task_id)?;

        // Self-heal: when the recorded folder still holds live sessions, resume
        // the one actually spawned for THIS task. The global search covers the
        // round that moved to a folder this task was never recorded in.
        let owned = newest_of([
            self.find_task_session_file(&meta.cwd, task_id, refs),
            self.find_task_session_anywhere(task_id, refs),
        ]);
        if let Some(found) = owned {
            let owned_id = found.session_id();
            if owned_id != meta.session_id {
                let _ = fs::create_dir_all(self.paths.task_dir(task_id));
                let _ = fs::copy(found.path(), self.archived_copy(task_id, &owned_id));
                meta = TaskArchive {
                    cwd: if found.cwd.is_empty() {
                        meta.cwd
                    } else {
                        found.cwd
                    },
                    session_id: owned_id,
                    session_file: found.file.path.to_string_lossy().into_owned(),
                    ..meta
                };
                self.write_meta(&meta);
            }
        }

        let live = if !meta.session_file.is_empty() {
            Some(PathBuf::from(&meta.session_file))
        } else if !meta.cwd.is_empty() {
            Some(
                self.paths
                    .project_transcripts(&meta.cwd)
                    .join(format!("{}.jsonl", meta.session_id)),
            )
        } else {
            None
        };

        // The self-healing above only works while the real session is still on
        // disk. When it is not, and what we hold is demonstrably another task's
        // conversation, refuse it rather than dropping the user into an
        // unrelated agent in an unrelated repo.
        let archived = self.archived_copy(task_id, &meta.session_id);
        let stored = match &live {
            Some(path) if path.exists() => path.clone(),
            _ => archived.clone(),
        };
        if stored.exists() && self.belongs_to_other_task(&stored, task_id, refs) {
            return None;
        }

        if let Some(live) = live.filter(|path| !path.exists()) {
            if archived.exists() {
                // The archive copy goes back where Claude Code expects it. The
                // parent of the recorded path, not of the recorded cwd: the two
                // differ once a round has moved, and the copy has to land beside
                // the session id `claude --resume` will look for.
                if let Some(parent) = live.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::copy(&archived, &live);
            }
        }
        Some(meta)
    }
}
