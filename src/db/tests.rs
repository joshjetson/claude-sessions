//! Ported from the Node app's `test/db.test.js`, plus the cases that only
//! became possible once the store was a value instead of a module global: a
//! corrupt database file, and the task-to-session index.
//!
//! Every test opens its own database under its own temp tree through
//! [`Paths::for_test`], so they run in parallel and none of them can reach the
//! user's real `~/.claude-sessions`. That is the structural version of the Node
//! suite's `helpers/isolate.js`, which had to be imported before anything else
//! to patch the environment in time.

mod archive;
mod daily_log;
mod import;
mod notifications;
mod resilience;
mod schema;

use super::*;
use crate::paths::Paths;
use crate::types::{Notification, NotificationKind, NotificationLevel, NotificationStatus};
use tempfile::TempDir;

/// A database in a throwaway tree. The directory is dropped with it.
struct TestDb {
    _dir: TempDir,
    paths: Paths,
    db: Db,
}

fn open() -> TestDb {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    let db = Db::open(&paths);
    assert!(
        db.available(),
        "temp database should open: {:?}",
        db.first_error()
    );
    TestDb {
        _dir: dir,
        paths,
        db,
    }
}

fn archive(task_id: i64, session_id: &str) -> TaskArchive {
    TaskArchive {
        task_id,
        cwd: "/repo/portal".to_string(),
        session_id: session_id.to_string(),
        session_file: format!("/x/{session_id}.jsonl"),
        archived_at: "2026-07-23T10:00:00.000Z".to_string(),
    }
}

fn log_entry(day: &str, ts: &str, task_id: i64, title: &str) -> DailyLogEntry {
    DailyLogEntry {
        day: day.to_string(),
        ts: ts.to_string(),
        task_id: Some(task_id),
        title: title.to_string(),
        summary: String::new(),
        short: "did a thing".to_string(),
        mr_url: None,
    }
}

fn notification(id: &str, ts: &str, title: &str) -> Notification {
    Notification {
        id: id.to_string(),
        ts: ts.to_string(),
        title: title.to_string(),
        message: String::new(),
        cwd: String::new(),
        project: "p".to_string(),
        session_id: None,
        task_id: None,
        level: NotificationLevel::Info,
        kind: NotificationKind::Info,
        status: NotificationStatus::Unread,
    }
}
