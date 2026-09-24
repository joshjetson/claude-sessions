//! The error log exists so errors stop landing on the dashboard. These pin the
//! parts that must hold for that to stay true: one error is one line, the file
//! cannot grow without bound, and the queue only fills while a dashboard asks
//! for it.

use super::*;
use crate::diagnostics::{KEEP_LINES, MAX_LOG_BYTES};

#[test]
fn the_log_lives_under_the_logs_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    assert_eq!(log_path(&paths), paths.logs_dir.join("errors.log"));
}

#[test]
fn a_line_carries_the_time_the_process_and_the_source() {
    let line = format_line(
        "2026-09-24T10:00:00.000Z",
        "dashboard",
        "kill",
        "no such process",
    );
    assert_eq!(
        line,
        "2026-09-24T10:00:00.000Z [dashboard] kill: no such process\n"
    );
}

#[test]
fn a_multi_line_message_stays_one_line() {
    let line = format_line("t", "daemon", "panic", "first\nsecond  \n\nthird\n");
    assert_eq!(line, "t [daemon] panic: first | second | third\n");
    assert_eq!(line.matches('\n').count(), 1);
}

#[test]
fn append_creates_the_logs_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("logs").join("errors.log");
    append(&log, "one\n").unwrap();
    append(&log, "two\n").unwrap();
    assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");
}

#[test]
fn append_trims_the_log_once_it_passes_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("errors.log");
    let line = format!("{}\n", "x".repeat(200));
    let body: String = std::iter::repeat_n(line, 2000).collect();
    assert!(body.len() as u64 > MAX_LOG_BYTES);
    fs::write(&log, &body).unwrap();

    append(&log, "newest error\n").unwrap();

    let trimmed = fs::read_to_string(&log).unwrap();
    assert_eq!(trimmed.lines().count(), KEEP_LINES);
    assert!((trimmed.len() as u64) < MAX_LOG_BYTES);
    // The newest line is the one a reader is looking for, so it survives.
    assert_eq!(trimmed.lines().last(), Some("newest error"));
}

#[test]
fn a_log_under_the_cap_keeps_every_line() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("errors.log");
    for n in 0..10 {
        append(&log, &format!("error {n}\n")).unwrap();
    }
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 10);
}

#[test]
fn the_queue_is_closed_unless_a_dashboard_opened_it() {
    // No unit test opens the queue, so a report from any test reaches nobody.
    let before = drain();
    report("test", "a report with no dashboard to show it");
    let after = drain();
    assert!(
        !after
            .iter()
            .chain(before.iter())
            .any(|r| r.message == "a report with no dashboard to show it"),
        "{after:?}"
    );
}

#[test]
fn an_open_queue_is_bounded() {
    let mut sink = Sink {
        path: None,
        label: "",
        queue: Some(Vec::new()),
    };
    for n in 0..(MAX_QUEUED + 5) {
        enqueue(&mut sink, "panic", &format!("error {n}"));
    }
    let queue = sink.queue.unwrap();
    assert_eq!(queue.len(), MAX_QUEUED);
    // The oldest go first, so the latest errors are the ones shown.
    assert_eq!(queue.first().unwrap().message, "error 5");
    assert_eq!(
        queue.last().unwrap().message,
        format!("error {}", MAX_QUEUED + 4)
    );
}
