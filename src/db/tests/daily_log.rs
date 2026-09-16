//! The daily log — the one store that answers questions across days.

use super::*;

fn seeded() -> TestDb {
    let t = open();
    t.db.put_daily_log_entry(&DailyLogEntry {
        day: "2026-08-01".to_string(),
        ts: "2026-08-01T10:00:00.000Z".to_string(),
        task_id: Some(100),
        title: "First".to_string(),
        summary: "full text".to_string(),
        short: "did a thing".to_string(),
        mr_url: Some("https://x/1".to_string()),
    });
    t.db.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T14:00:00.000Z",
        101,
        "Second",
    ));
    t.db.put_daily_log_entry(&log_entry(
        "2026-08-02",
        "2026-08-02T09:00:00.000Z",
        100,
        "First again",
    ));
    t
}

#[test]
fn returns_a_day_in_chronological_order() {
    let t = seeded();
    let rows = t.db.daily_log_entries(Some("2026-08-01"));
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|r| r.task_id).collect::<Vec<_>>(),
        vec![Some(100), Some(101)]
    );
    assert_eq!(rows[0].mr_url.as_deref(), Some("https://x/1"));
    assert_eq!(rows[0].summary, "full text");
}

#[test]
fn without_a_day_every_entry_comes_back() {
    let t = seeded();
    assert_eq!(t.db.daily_log_entries(None).len(), 3);
    assert!(t.db.daily_log_entries(Some("2026-12-25")).is_empty());
}

#[test]
fn lists_distinct_days_in_order() {
    let t = seeded();
    let days = t.db.list_log_days();
    assert_eq!(days, vec!["2026-08-01", "2026-08-02"]);
}

#[test]
fn task_history_spans_days() {
    // The thing per-day markdown cannot answer: the same task on two days is
    // two files, and nothing joins them.
    let t = seeded();
    let history = t.db.task_history(100);
    assert_eq!(history.len(), 2);
    assert_eq!(
        history.iter().map(|h| h.day.as_str()).collect::<Vec<_>>(),
        vec!["2026-08-01", "2026-08-02"]
    );
}

#[test]
fn a_task_with_no_entries_returns_an_empty_list() {
    let t = seeded();
    assert!(t.db.task_history(424_242).is_empty());
}

#[test]
fn entries_are_appended_not_replaced() {
    // Unlike the archive, a second entry for the same task on the same day is a
    // second row — two tasks can ship twice in a day.
    let t = open();
    t.db.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T10:00:00.000Z",
        100,
        "One",
    ));
    t.db.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T11:00:00.000Z",
        100,
        "Two",
    ));
    assert_eq!(t.db.task_history(100).len(), 2);
}
