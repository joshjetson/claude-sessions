//! The notification feed, and the `alerted` keys that keep it from repeating
//! itself.
//!
//! Both belong to the same job: tell the user something once. The feed is what
//! survives a dashboard restart — "what did I miss" — and `alerted` is why a
//! daemon restart does not re-announce every task that was already sitting in
//! the watched stage.

use rusqlite::{Connection, Row};

use super::Db;
use crate::types::{Notification, NotificationKind, NotificationLevel, NotificationStatus};
use crate::util::iso_now;

/// How many notifications the daemon restores into the feed on startup.
pub const RECENT_LIMIT: i64 = 200;
/// How many rows survive a prune. The feed is a log, not an archive.
pub const PRUNE_KEEP: i64 = 1000;

const NOTIFICATION_COLUMNS: &str =
    "id, ts, title, message, cwd, project, session_id, task_id, level, status, kind, run_id";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        id: row.get("id")?,
        ts: row.get("ts")?,
        title: row.get("title")?,
        message: row.get("message")?,
        cwd: row.get("cwd")?,
        project: row.get("project")?,
        // The column defaults to '' rather than NULL, so an absent session
        // arrives as an empty string; keep `None` meaning absent.
        session_id: Some(row.get::<_, String>("session_id")?).filter(|s| !s.is_empty()),
        task_id: row.get("task_id")?,
        level: NotificationLevel::from_label(&row.get::<_, String>("level")?),
        kind: NotificationKind::from_label(&row.get::<_, String>("kind")?),
        run_id: row.get("run_id")?,
        status: NotificationStatus::from_label(&row.get::<_, String>("status")?),
    })
}

impl Db {
    /// Stores a notification. Re-inserting the same id updates its status and
    /// nothing else — the daemon replays notifications it already holds, and
    /// the stored text is the original.
    pub fn put_notification(&self, n: &Notification) {
        self.exec("put_notification", |conn| {
            conn.prepare_cached(
                "INSERT INTO notifications
                   (id, ts, title, message, cwd, project, session_id, task_id, level, status, kind, run_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET status = excluded.status",
            )?
            .execute((
                &n.id,
                &n.ts,
                &n.title,
                &n.message,
                &n.cwd,
                &n.project,
                n.session_id.as_deref().unwrap_or(""),
                n.task_id,
                n.level.as_str(),
                n.status.as_str(),
                n.kind.as_str(),
                &n.run_id,
            ))?;
            Ok(())
        });
    }

    /// Newest first — the order the feed is read in.
    pub fn recent_notifications(&self, limit: i64) -> Vec<Notification> {
        self.rows(
            "recent_notifications",
            &format!("SELECT {NOTIFICATION_COLUMNS} FROM notifications ORDER BY ts DESC LIMIT ?1"),
            [limit],
            from_row,
        )
    }

    /// Sets a status on specific ids. An empty list is a no-op, deliberately:
    /// the daemon's status endpoint takes its ids from a request body, and
    /// "mark everything read" must be asked for by name, not by omission.
    pub fn set_notification_status<S: AsRef<str>>(&self, ids: &[S], status: NotificationStatus) {
        self.each_id(
            "set_notification_status",
            "UPDATE notifications SET status = ?1 WHERE id = ?2",
            ids,
            Some(status.as_str()),
        );
    }

    /// Removes rows outright — what "dismiss" means.
    pub fn delete_notifications<S: AsRef<str>>(&self, ids: &[S]) {
        self.each_id(
            "delete_notifications",
            "DELETE FROM notifications WHERE id = ?1",
            ids,
            None,
        );
    }

    /// Marks the named notifications read, or the whole feed when no ids are
    /// given. The empty case is the "mark all read" key in the board view.
    pub fn mark_notifications_read<S: AsRef<str>>(&self, ids: &[S]) {
        if ids.is_empty() {
            self.exec("mark_notifications_read", |conn| {
                conn.execute("UPDATE notifications SET status = 'read'", [])?;
                Ok(())
            });
            return;
        }
        self.set_notification_status(ids, NotificationStatus::Read);
    }

    /// Keeps the table from growing without bound, newest `keep` rows surviving.
    pub fn prune_notifications(&self, keep: i64) {
        self.exec("prune_notifications", |conn| {
            conn.prepare_cached(
                "DELETE FROM notifications WHERE id NOT IN (
                   SELECT id FROM notifications ORDER BY ts DESC LIMIT ?1
                 )",
            )?
            .execute([keep])?;
            Ok(())
        });
    }

    /// Whether this alert key has already fired. The keys are built by the
    /// alert rules (`assigned:<task>:<stage>`), not here.
    pub fn was_alerted(&self, key: &str) -> bool {
        self.one(
            "was_alerted",
            "SELECT 1 FROM alerted WHERE key = ?1",
            [key],
            |row| row.get::<_, i64>(0),
        )
        .is_some()
    }

    pub fn mark_alerted(&self, key: &str) {
        self.exec("mark_alerted", |conn| {
            conn.prepare_cached("INSERT OR REPLACE INTO alerted (key, ts) VALUES (?1, ?2)")?
                .execute((key, iso_now()))?;
            Ok(())
        });
    }

    /// One statement per id, in one transaction. `status` is bound as `?1` when
    /// the statement takes one, so the update and the delete share this.
    fn each_id<S: AsRef<str>>(&self, what: &str, sql: &str, ids: &[S], status: Option<&str>) {
        if ids.is_empty() {
            return;
        }
        self.exec(what, |conn: &Connection| {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare_cached(sql)?;
                for id in ids {
                    match status {
                        Some(status) => stmt.execute((status, id.as_ref()))?,
                        None => stmt.execute((id.as_ref(),))?,
                    };
                }
            }
            tx.commit()
        });
    }
}
