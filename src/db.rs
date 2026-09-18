//! SQLite store for the metadata that used to live as loose files under
//! `~/.claude-sessions`: the task archive, the daily log, the notification feed
//! and alert bookkeeping.
//!
//! Transcripts stay on disk as `.jsonl` — they are large, append-only, and
//! handed straight to `claude --resume`. What moves in here is everything we
//! QUERY: "which tasks have an archive", "what shipped on 2026-08-14", "what did
//! I miss while the dashboard was closed", "where is task 6117's transcript".
//!
//! # A broken database is never fatal
//!
//! The files are still written alongside (`meta.json`, `logs/*.md`), so every
//! accessor here can fail soft: a database that will not open, or a statement
//! that errors, yields the neutral value — `None`, an empty `Vec`, `false` — and
//! the caller falls back to the filesystem exactly as the Node app did. Nothing
//! in this module returns a `Result` or panics. The first failure is kept in
//! [`Db::first_error`] so a daemon can report it once; it is deliberately not
//! printed, because the TUI owns the terminal.
//!
//! # Concurrency
//!
//! The daemon refreshes on several timers from more than one thread, so the
//! connection sits behind a `Mutex`. A pool would buy nothing: SQLite serialises
//! writers anyway, every statement here is a keyed lookup or a single-row write,
//! and one connection keeps the WAL file count down. A poisoned lock is taken
//! over rather than propagated — a panic in one accessor must not disable the
//! store for the rest of the process. Share it as `Arc<Db>`.

mod import;
mod log;
mod notifications;
mod qa_runs;
mod tasks;

#[cfg(test)]
mod tests;

pub use import::{log_day_from_filename, parse_log_line, ImportReport, LogLine};
pub use log::DailyLogEntry;
pub use notifications::{PRUNE_KEEP, RECENT_LIMIT};
pub use tasks::{TaskArchive, TaskSession};

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use rusqlite::{Connection, Params, Row};

use crate::paths::Paths;

/// Applied in order, once each; `PRAGMA user_version` records how far we got.
///
/// **This list is shared with the Node app, which writes the same database
/// file.** `PRAGMA user_version` is one integer for both, so a migration that
/// exists here at index N and means something different there at index N is a
/// silent corruption: whichever app opens the file first sets the version, and
/// the other skips its own migration without running it and without erroring.
/// The symptom is never a crash — it is an INSERT against a column that was
/// never added, swallowed by a catch, and a feature that quietly does nothing.
///
/// So: append only, never renumber, and add the same SQL at the same index in
/// the Node app's `src/db.ts`. Migrations 1 and 2 are the Node app's original
/// schema verbatim, which is why a database it wrote is already at version 2.
const MIGRATIONS: [&str; 5] = [
    // 1 — initial schema.
    "
    CREATE TABLE IF NOT EXISTS task_archive (
      task_id      INTEGER PRIMARY KEY,
      cwd          TEXT NOT NULL DEFAULT '',
      session_id   TEXT NOT NULL DEFAULT '',
      session_file TEXT NOT NULL DEFAULT '',
      archived_at  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS daily_log (
      id       INTEGER PRIMARY KEY AUTOINCREMENT,
      day      TEXT    NOT NULL,
      ts       TEXT    NOT NULL,
      task_id  INTEGER,
      title    TEXT,
      summary  TEXT,
      short    TEXT,
      mr_url   TEXT
    );
    CREATE INDEX IF NOT EXISTS daily_log_day_idx  ON daily_log(day);
    CREATE INDEX IF NOT EXISTS daily_log_task_idx ON daily_log(task_id);

    CREATE TABLE IF NOT EXISTS notifications (
      id         TEXT PRIMARY KEY,
      ts         TEXT NOT NULL,
      title      TEXT NOT NULL DEFAULT '',
      message    TEXT NOT NULL DEFAULT '',
      cwd        TEXT NOT NULL DEFAULT '',
      project    TEXT NOT NULL DEFAULT '',
      session_id TEXT NOT NULL DEFAULT '',
      task_id    INTEGER,
      level      TEXT NOT NULL DEFAULT 'info',
      status     TEXT NOT NULL DEFAULT 'unread'
    );
    CREATE INDEX IF NOT EXISTS notifications_ts_idx ON notifications(ts DESC);
    ",
    // 2 — alert bookkeeping, so a daemon restart does not re-announce every task
    // that was already sitting in the watched stage.
    "
    CREATE TABLE IF NOT EXISTS alerted (
      key TEXT PRIMARY KEY,
      ts  TEXT NOT NULL
    );
    ",
    // 3 — NEW IN THE PORT. Which transcript belongs to which task, kept as a
    // keyed row instead of being rediscovered by brute force.
    //
    // The Node app answered "where is this task's session?" by reading every
    // project directory, stat-ing every `.jsonl` in it and then reading a 128KB
    // head from up to 400 of them — on every session that vanished. One SELECT
    // replaces the scan. The reverse index exists for the one-session-one-task
    // rule: before a pending launch claims a transcript, the daemon has to know
    // whether another task already holds it.
    "
    CREATE TABLE IF NOT EXISTS task_session_index (
      task_id      INTEGER PRIMARY KEY,
      session_file TEXT NOT NULL DEFAULT '',
      cwd          TEXT NOT NULL DEFAULT '',
      updated_at   TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS task_session_file_idx ON task_session_index(session_file);
    ",
    // 4 — what a notification IS, not just how loud it is.
    //
    // `level` says how to announce something. It does not say whether the
    // sender is blocked waiting for an answer, and a QA run has to know: an
    // agent stopped at a question is the one thing costing the reviewer time,
    // and a run cannot count those without a field that distinguishes them.
    //
    // Existing rows default to 'info', which is what they were.
    "
    ALTER TABLE notifications ADD COLUMN kind TEXT NOT NULL DEFAULT 'info';
    CREATE INDEX IF NOT EXISTS notifications_kind_idx ON notifications(kind);
    ",
    // 5 — QA runs, so one survives quitting the dashboard.
    //
    // A run was memory only. Quitting ended the process and the grouping went
    // with it: the coordinator and its agents kept running, but on restart the
    // dashboard no longer knew they belonged together, so every one of them
    // fell back to its project and the Runs section vanished. Nothing was
    // broken except the dashboard's memory of it.
    //
    // `task_ids` and `spawned` are comma-separated rather than a child table.
    // A run holds a handful of ids, they are always read and written whole, and
    // a second table would need its own migration to say nothing more.
    "
    CREATE TABLE IF NOT EXISTS qa_runs (
      id                  TEXT PRIMARY KEY,
      project_name        TEXT NOT NULL DEFAULT '',
      stage_name          TEXT NOT NULL DEFAULT '',
      task_ids            TEXT NOT NULL DEFAULT '',
      spawned             TEXT NOT NULL DEFAULT '',
      started_at          TEXT NOT NULL DEFAULT '',
      lane_limit          INTEGER,
      mode                TEXT NOT NULL DEFAULT 'triage',
      coordinator_started INTEGER NOT NULL DEFAULT 0
    );
    ",
];

/// The store. Build one, share it; see the module docs for the error posture.
#[derive(Debug)]
pub struct Db {
    path: PathBuf,
    /// `None` once opening or migrating failed — the whole store then answers
    /// neutrally rather than making every caller handle it.
    conn: Option<Mutex<Connection>>,
    first_error: OnceLock<String>,
}

impl Db {
    /// Opens (creating it if need be) and migrates. Never fails.
    pub fn open(paths: &Paths) -> Db {
        Db::open_at(&paths.db_path)
    }

    /// The same against an explicit file, which is what `CLAUDE_SESSIONS_DB`
    /// and the tests want.
    pub fn open_at(path: &Path) -> Db {
        let db = Db {
            path: path.to_path_buf(),
            conn: None,
            first_error: OnceLock::new(),
        };
        match connect(path) {
            Ok(conn) => Db {
                conn: Some(Mutex::new(conn)),
                ..db
            },
            Err(err) => {
                db.trace("open", &err);
                db
            }
        }
    }

    /// False when the database could not be opened or migrated. Callers use it
    /// to decide whether to bother, not to decide whether to continue.
    pub fn available(&self) -> bool {
        self.conn.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The first failure this store hit, for a daemon to log once at startup.
    pub fn first_error(&self) -> Option<&str> {
        self.first_error.get().map(String::as_str)
    }

    /// How far the migrations got. 0 when the database is unavailable.
    pub fn schema_version(&self) -> i64 {
        self.one("schema_version", "PRAGMA user_version", [], |row| {
            row.get(0)
        })
        .unwrap_or(0)
    }

    // --- the three shapes every accessor is built from ----------------------

    /// Runs `f` against the connection, turning any failure into `None` and
    /// recording it once. `what` names the accessor in that record.
    pub(crate) fn with_conn<T>(
        &self,
        what: &str,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Option<T> {
        let guard = self.lock()?;
        match f(&guard) {
            Ok(value) => Some(value),
            Err(err) => {
                self.trace(what, &err);
                None
            }
        }
    }

    /// A statement run for its effect. Writes are fire-and-forget: the Node
    /// accessors swallowed errors the same way, and the file written beside the
    /// row is the durable copy.
    pub(crate) fn exec(&self, what: &str, f: impl FnOnce(&Connection) -> rusqlite::Result<()>) {
        self.with_conn(what, f);
    }

    /// One row, or `None` — a missing row and a broken database are the same
    /// answer to the caller.
    pub(crate) fn one<T, P, F>(&self, what: &str, sql: &str, params: P, map: F) -> Option<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.with_conn(what, |conn| {
            let mut stmt = conn.prepare_cached(sql)?;
            match stmt.query_row(params, map) {
                Ok(value) => Ok(Some(value)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(err) => Err(err),
            }
        })
        .flatten()
    }

    /// Every matching row. A row that will not map is skipped rather than
    /// discarding the rest, which is what the Node accessors' per-row
    /// `Number.isFinite` filtering amounted to.
    pub(crate) fn rows<T, P, F>(&self, what: &str, sql: &str, params: P, map: F) -> Vec<T>
    where
        P: Params,
        F: Fn(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.with_conn(what, |conn| {
            let mut stmt = conn.prepare_cached(sql)?;
            let rows = stmt.query_map(params, &map)?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default()
    }

    fn lock(&self) -> Option<MutexGuard<'_, Connection>> {
        Some(
            self.conn
                .as_ref()?
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Kept, not printed, and only the first one: a failing database would
    /// otherwise emit a line per accessor per tick, and the dashboard's
    /// alternate screen is no place for it.
    fn trace(&self, what: &str, err: &dyn std::fmt::Display) {
        let _ = self.first_error.set(format!("{what}: {err}"));
    }
}

fn connect(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    // WAL keeps the daemon's writes from blocking a concurrent reader (the
    // journal viewer, a second dashboard); the busy timeout covers the writer
    // lock they still contend for. Set through `execute_batch` because
    // `journal_mode` answers with a row.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 3000;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

/// Applies whatever [`MIGRATIONS`] the file has not seen. Running it against an
/// already-migrated database does nothing, and a database from a newer version
/// is left alone rather than being downgraded.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = usize::try_from(version).unwrap_or(0);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        conn.execute_batch(sql)?;
        // No bind parameters in a PRAGMA; `index` is ours, not user input.
        conn.execute_batch(&format!("PRAGMA user_version = {}", index + 1))?;
    }
    Ok(())
}
