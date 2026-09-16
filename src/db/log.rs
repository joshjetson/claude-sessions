//! `daily_log` — one row per completed task per day.
//!
//! The markdown file under `logs/` is the human artifact and stays the thing the
//! log viewer renders. The row is what makes an entry answerable: per-task
//! history across days is a question the per-day files cannot answer at all,
//! because the same task appearing on three days is three separate files.

use rusqlite::Row;
use serde::{Deserialize, Serialize};

use super::Db;

/// One line of a daily log. `summary` is the agent's full sign-off text and
/// `short` the standup-sized condensation of it; the markdown only ever carried
/// the short form, so rows imported from a file have `summary` empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyLogEntry {
    /// `YYYY-MM-DD`, the day the file is named after.
    pub day: String,
    /// ISO-8601; the sort key within a day.
    pub ts: String,
    pub task_id: Option<i64>,
    pub title: String,
    pub summary: String,
    pub short: String,
    pub mr_url: Option<String>,
}

impl DailyLogEntry {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<DailyLogEntry> {
        Ok(DailyLogEntry {
            day: row.get("day")?,
            ts: row.get("ts")?,
            task_id: row.get("task_id")?,
            // These four are nullable in the schema the Node app created, so an
            // older row can legitimately hold NULL where we want a string.
            title: row.get::<_, Option<String>>("title")?.unwrap_or_default(),
            summary: row.get::<_, Option<String>>("summary")?.unwrap_or_default(),
            short: row.get::<_, Option<String>>("short")?.unwrap_or_default(),
            mr_url: row.get("mr_url")?,
        })
    }
}

/// Columns in the order [`DailyLogEntry::from_row`] reads them by name.
const LOG_COLUMNS: &str = "day, ts, task_id, title, summary, short, mr_url";

impl Db {
    /// Appends an entry. Unlike the archive this never replaces: a task worked
    /// on three days is three rows, which is the whole point of the table.
    pub fn put_daily_log_entry(&self, entry: &DailyLogEntry) {
        self.exec("put_daily_log_entry", |conn| {
            conn.prepare_cached(
                "INSERT INTO daily_log (day, ts, task_id, title, summary, short, mr_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute((
                &entry.day,
                &entry.ts,
                entry.task_id,
                &entry.title,
                &entry.summary,
                &entry.short,
                &entry.mr_url,
            ))?;
            Ok(())
        });
    }

    /// Entries for one day, or every entry ever when `day` is `None` — one
    /// function because the callers differ only in whether they have a day.
    pub fn daily_log_entries(&self, day: Option<&str>) -> Vec<DailyLogEntry> {
        match day {
            Some(day) => self.rows(
                "daily_log_entries",
                &format!("SELECT {LOG_COLUMNS} FROM daily_log WHERE day = ?1 ORDER BY ts"),
                [day],
                DailyLogEntry::from_row,
            ),
            None => self.rows(
                "daily_log_entries",
                &format!("SELECT {LOG_COLUMNS} FROM daily_log ORDER BY ts"),
                [],
                DailyLogEntry::from_row,
            ),
        }
    }

    /// Every entry ever recorded for a task, oldest first — the history the
    /// per-day markdown files cannot answer.
    pub fn task_history(&self, task_id: i64) -> Vec<DailyLogEntry> {
        self.rows(
            "task_history",
            &format!("SELECT {LOG_COLUMNS} FROM daily_log WHERE task_id = ?1 ORDER BY ts"),
            [task_id],
            DailyLogEntry::from_row,
        )
    }

    /// Days that have at least one entry, oldest first. The log viewer unions
    /// this with the days it finds on disk, so a file that was moved or deleted
    /// still shows up.
    pub fn list_log_days(&self) -> Vec<String> {
        self.rows(
            "list_log_days",
            "SELECT DISTINCT day FROM daily_log ORDER BY day",
            [],
            |row| row.get(0),
        )
    }
}
