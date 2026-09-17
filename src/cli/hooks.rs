//! The two engine hooks the daemon supplies: the plan-usage check and the
//! standup log.
//!
//! They live here rather than in [`crate::daemon`] because they are policy —
//! where the check runs, what it does with a failure, which stores a finished
//! task is written to — and the engine only carries the slots.

use crate::daemon::{DailyLogHook, UsageHook};
use crate::dailylog::{append_daily_log, LogRequest};
use crate::db::Db;
use crate::paths::Paths;
use crate::term::{Exec, SpawnPolicy};
use crate::usage::{fetch_usage, UsageSnapshot, FETCH_TIMEOUT};

/// The plan-usage check the daemon owns.
///
/// It runs in `$HOME` so a repository's `CLAUDE.md` cannot change what a usage
/// check reports, and it keeps the previous reading so a failed check leaves
/// stale numbers on screen rather than blanking the header.
pub(super) fn usage_hook(paths: &Paths, spawn: SpawnPolicy) -> UsageHook {
    let home = paths.home.clone();
    let previous: std::sync::Mutex<Option<UsageSnapshot>> = std::sync::Mutex::new(None);
    Box::new(move || {
        let fresh = fetch_usage(&Exec::new(spawn), &home, FETCH_TIMEOUT);
        let mut held = match previous.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        let merged = fresh.merge_over(held.as_ref());
        *held = Some(merged.clone());
        serde_json::to_value(&merged).unwrap_or(serde_json::Value::Null)
    })
}

/// The standup line a finished task contributes: one markdown line, one row.
pub(super) fn daily_log_hook(paths: &Paths) -> DailyLogHook {
    let paths = paths.clone();
    Box::new(move |record| {
        // A connection of its own rather than the engine's: the hook is called
        // from the completion path and the engine's handle is not lent out.
        let db = Db::open(&paths);
        append_daily_log(
            &paths,
            Some(&db),
            &LogRequest {
                task_id: record.task_id,
                title: record.title.clone(),
                summary: record.summary.clone(),
                mr_url: record.mr_url.clone(),
            },
            chrono::Local::now(),
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_hook_keeps_the_previous_reading_when_a_check_fails() {
        // Under a refusing policy every check fails, which is exactly the shape
        // of a timeout: the hook must answer with the last numbers marked
        // stale rather than blanking the header.
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(tmp.path());
        let hook = usage_hook(&paths, SpawnPolicy::Refuse);

        let first: UsageSnapshot = serde_json::from_value(hook()).unwrap();
        assert!(!first.ok);
        assert_eq!(first.session, None, "a refusal is not a reading of zero");

        let again: UsageSnapshot = serde_json::from_value(hook()).unwrap();
        assert!(!again.ok);
        assert!(again.error.is_some());
    }

    #[test]
    fn the_daily_log_hook_writes_both_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(tmp.path());
        let hook = daily_log_hook(&paths);
        hook(&crate::daemon::DailyLogRecord {
            task_id: 6137,
            title: "Widget alignment".into(),
            summary: "**Fix:** tightened the guard so it reads the session first.".into(),
            mr_url: Some("https://git.example.com/mr/7".into()),
        });

        let day = crate::dailylog::ymd(chrono::Local::now());
        let markdown =
            std::fs::read_to_string(crate::dailylog::daily_log_path(&paths, &day)).unwrap();
        assert!(
            markdown.contains("- **#6137 Widget alignment** —"),
            "{markdown}"
        );
        assert!(markdown.contains("tightened the guard"), "{markdown}");
        assert!(
            markdown.contains("([MR](https://git.example.com/mr/7))"),
            "{markdown}"
        );

        let rows = Db::open(&paths).task_history(6137);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].short,
            "tightened the guard so it reads the session first."
        );
        assert!(
            rows[0].summary.contains("**Fix:**"),
            "the full text is kept"
        );
    }
}
