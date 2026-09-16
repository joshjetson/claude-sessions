//! `task_archive` — which finished tasks have a transcript kept for
//! `claude --resume` — and `task_session_index`, which is where that transcript
//! is right now.
//!
//! They look alike and are deliberately separate. The archive is a record of
//! something that happened, written once when a task finishes. The index is a
//! cache of the current answer to "where does this task's conversation live",
//! maintained as sessions are discovered and moved (QA runs relocate a task's
//! work to a different folder entirely), and safe to rebuild from the
//! filesystem at any time.

use rusqlite::Row;
use serde::{Deserialize, Serialize};

use super::Db;

/// One archived task. The field names are also `meta.json`'s, so the row and
/// the file beside it are the same shape — the file stays the fallback for a
/// database that will not open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArchive {
    pub task_id: i64,
    pub cwd: String,
    pub session_id: String,
    pub session_file: String,
    pub archived_at: String,
}

impl TaskArchive {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<TaskArchive> {
        Ok(TaskArchive {
            task_id: row.get("task_id")?,
            cwd: row.get("cwd")?,
            session_id: row.get("session_id")?,
            session_file: row.get("session_file")?,
            archived_at: row.get("archived_at")?,
        })
    }
}

/// Where a task's live transcript was last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSession {
    pub task_id: i64,
    pub session_file: String,
    pub cwd: String,
    pub updated_at: String,
}

impl TaskSession {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<TaskSession> {
        Ok(TaskSession {
            task_id: row.get("task_id")?,
            session_file: row.get("session_file")?,
            cwd: row.get("cwd")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl Db {
    /// Records an archive, replacing any earlier one for the task. Re-archiving
    /// is normal — a revision round produces a newer transcript for a task that
    /// already has one, and the newest is the one to resume.
    pub fn put_task_archive(&self, row: &TaskArchive) {
        self.exec("put_task_archive", |conn| {
            conn.prepare_cached(
                "INSERT INTO task_archive (task_id, cwd, session_id, session_file, archived_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(task_id) DO UPDATE SET
                   cwd = excluded.cwd,
                   session_id = excluded.session_id,
                   session_file = excluded.session_file,
                   archived_at = excluded.archived_at",
            )?
            .execute((
                row.task_id,
                &row.cwd,
                &row.session_id,
                &row.session_file,
                &row.archived_at,
            ))?;
            Ok(())
        });
    }

    pub fn get_task_archive(&self, task_id: i64) -> Option<TaskArchive> {
        self.one(
            "get_task_archive",
            "SELECT task_id, cwd, session_id, session_file, archived_at
             FROM task_archive WHERE task_id = ?1",
            [task_id],
            TaskArchive::from_row,
        )
    }

    /// Every archived task id. The daemon seeds its in-memory set from this at
    /// startup, which is why it is ordered: a stable order keeps the board's
    /// archive badges from reshuffling between restarts.
    pub fn list_archived_task_ids(&self) -> Vec<i64> {
        self.rows(
            "list_archived_task_ids",
            "SELECT task_id FROM task_archive ORDER BY task_id",
            [],
            |row| row.get(0),
        )
    }

    /// Points a task at its transcript, replacing whatever was there.
    pub fn put_task_session(&self, row: &TaskSession) {
        self.exec("put_task_session", |conn| {
            conn.prepare_cached(
                "INSERT INTO task_session_index (task_id, session_file, cwd, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(task_id) DO UPDATE SET
                   session_file = excluded.session_file,
                   cwd = excluded.cwd,
                   updated_at = excluded.updated_at",
            )?
            .execute((row.task_id, &row.session_file, &row.cwd, &row.updated_at))?;
            Ok(())
        });
    }

    /// The indexed answer to "where is this task's transcript" — one keyed
    /// lookup in place of reading every project directory on disk.
    pub fn task_session(&self, task_id: i64) -> Option<TaskSession> {
        self.one(
            "task_session",
            "SELECT task_id, session_file, cwd, updated_at
             FROM task_session_index WHERE task_id = ?1",
            [task_id],
            TaskSession::from_row,
        )
    }

    /// The reverse: which task holds this transcript. A pending launch must
    /// refuse a session another task already owns, and this answers that
    /// without opening the file.
    pub fn task_for_session_file(&self, session_file: &str) -> Option<TaskSession> {
        if session_file.is_empty() {
            return None;
        }
        self.one(
            "task_for_session_file",
            "SELECT task_id, session_file, cwd, updated_at
             FROM task_session_index WHERE session_file = ?1",
            [session_file],
            TaskSession::from_row,
        )
    }
}
