//! The notification feed and the alert keys beside it.

use super::*;

#[test]
fn persists_and_returns_newest_first() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.put_notification(&notification("n2", "2026-08-01T12:00:00.000Z", "newer"));

    let titles: Vec<String> =
        t.db.recent_notifications(50)
            .into_iter()
            .map(|n| n.title)
            .collect();
    assert_eq!(
        titles,
        vec!["newer", "older"],
        "newest-first ordering is wrong"
    );
}

#[test]
fn a_full_notification_round_trips() {
    let t = open();
    let mut n = notification("n1", "2026-08-01T10:00:00.000Z", "MR failed to merge");
    n.message = "conflict in app/models".to_string();
    n.cwd = "/repo/portal".to_string();
    n.session_id = Some("sess-a".to_string());
    n.task_id = Some(5944);
    n.level = NotificationLevel::Error;
    n.status = NotificationStatus::Resolved;
    t.db.put_notification(&n);

    assert_eq!(t.db.recent_notifications(10), vec![n]);
}

#[test]
fn an_absent_session_comes_back_absent_not_empty() {
    // The column defaults to '' rather than NULL, so this is the round trip
    // that would otherwise turn None into Some("").
    let t = open();
    t.db.put_notification(&notification(
        "n1",
        "2026-08-01T10:00:00.000Z",
        "no session",
    ));
    assert_eq!(t.db.recent_notifications(10)[0].session_id, None);
}

#[test]
fn re_inserting_the_same_id_updates_status_instead_of_duplicating() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));

    let mut again = notification("n1", "2026-08-01T10:00:00.000Z", "rewritten title");
    again.status = NotificationStatus::Read;
    t.db.put_notification(&again);

    let rows = t.db.recent_notifications(10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, NotificationStatus::Read);
    // Only the status is updated — the text is the original notification's.
    assert_eq!(rows[0].title, "older");
}

#[test]
fn marks_specific_ids_read_and_leaves_the_rest_alone() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.put_notification(&notification("n2", "2026-08-01T12:00:00.000Z", "newer"));

    t.db.mark_notifications_read(&["n2"]);
    let rows = t.db.recent_notifications(10);
    assert_eq!(rows[0].status, NotificationStatus::Read);
    assert_eq!(rows[1].status, NotificationStatus::Unread);
}

#[test]
fn marks_the_whole_feed_read_when_no_ids_are_given() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.put_notification(&notification("n2", "2026-08-01T12:00:00.000Z", "newer"));

    t.db.mark_notifications_read::<&str>(&[]);
    assert!(t
        .db
        .recent_notifications(10)
        .iter()
        .all(|n| n.status == NotificationStatus::Read));
}

#[test]
fn an_explicit_status_applies_only_to_the_ids_named() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.put_notification(&notification("n2", "2026-08-01T12:00:00.000Z", "newer"));

    t.db.set_notification_status(&["n1".to_string()], NotificationStatus::Resolved);
    // An empty list must NOT be read as "all of them" — the daemon's status
    // endpoint takes its ids from a request body.
    t.db.set_notification_status::<&str>(&[], NotificationStatus::Deleted);

    let rows = t.db.recent_notifications(10);
    assert_eq!(rows[0].status, NotificationStatus::Unread);
    assert_eq!(rows[1].status, NotificationStatus::Resolved);
}

#[test]
fn dismissing_removes_the_rows() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.put_notification(&notification("n2", "2026-08-01T12:00:00.000Z", "newer"));

    t.db.delete_notifications(&["n1"]);
    let rows = t.db.recent_notifications(10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "n2");
}

#[test]
fn numeric_ids_survive_the_round_trip() {
    let t = open();
    t.db.put_notification(&notification(
        "12345",
        "2026-08-03T10:00:00.000Z",
        "numeric",
    ));

    let rows = t.db.recent_notifications(10);
    assert_eq!(rows[0].id, "12345");
    // …and stay text, rather than being coerced by column affinity into an
    // integer that no longer matches the id the daemon holds.
    let kind: String =
        t.db.one(
            "t",
            "SELECT typeof(id) FROM notifications WHERE id = ?1",
            ["12345"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "text");
}

#[test]
fn an_unknown_level_or_status_reads_as_the_column_default() {
    let t = open();
    t.db.put_notification(&notification("n1", "2026-08-01T10:00:00.000Z", "older"));
    t.db.exec("t", |conn| {
        conn.execute_batch("UPDATE notifications SET level = 'catastrophe', status = 'snoozed'")
    });

    let rows = t.db.recent_notifications(10);
    assert_eq!(rows[0].level, NotificationLevel::Info);
    assert_eq!(rows[0].status, NotificationStatus::Unread);
}

#[test]
fn pruning_keeps_only_the_newest_n() {
    let t = open();
    for i in 0..20 {
        t.db.put_notification(&notification(
            &format!("bulk-{i}"),
            &format!("2026-09-{:02}T10:00:00.000Z", i + 1),
            &format!("bulk {i}"),
        ));
    }
    t.db.prune_notifications(5);

    let rows = t.db.recent_notifications(100);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].title, "bulk 19", "pruning kept the wrong end");
    assert_eq!(rows[4].title, "bulk 15");
}

#[test]
fn an_alert_key_fires_once() {
    let t = open();
    assert!(!t.db.was_alerted("assigned:5944:approved to start"));

    t.db.mark_alerted("assigned:5944:approved to start");
    assert!(t.db.was_alerted("assigned:5944:approved to start"));
    // A different stage is a different key — a task that leaves and returns is
    // announced again.
    assert!(!t.db.was_alerted("assigned:5944:in progress"));

    // Marking twice is not an error; the row is replaced.
    t.db.mark_alerted("assigned:5944:approved to start");
    assert!(t.db.was_alerted("assigned:5944:approved to start"));
}
