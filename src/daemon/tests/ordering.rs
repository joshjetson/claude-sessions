//! Where a session sits in its project's list, and why it must not move.
//!
//! The rule is that a row keeps its place for as long as the session lives.
//! Sorting by activity puts whichever agent wrote most recently at the top and
//! shifts every row below it, about once a second with several agents running —
//! so you cannot point at a row, and a keypress lands on a different session
//! than the one you aimed at.
//!
//! The regression these tests exist for: the sort read `lstart` with an
//! RFC3339-only parser. `ps` prints `Thu Sep 17 21:29:46 2026`, which is not
//! RFC3339, so the parse failed for EVERY session and the sort fell through to
//! the transcript mtime — which is the activity ordering it was written to
//! replace. The code looked correct and did the opposite of what it said.

use super::*;

/// A session started at a BSD `ps -o lstart` time, last written at `mtime`.
fn started(id: &str, lstart: &str, mtime_secs: u64) -> Session {
    Session {
        lstart: Some(lstart.to_string()),
        session_mtime: UNIX_EPOCH + Duration::from_secs(mtime_secs),
        ..a_session(id, "/repo/alpha")
    }
}

#[test]
fn sessions_sort_by_ps_start_time_not_by_last_write() {
    // "bbb" started first but "aaa" wrote most recently. The mtimes are
    // deliberately the OPPOSITE of the start times, so an implementation that
    // falls back to mtime produces the wrong order rather than the right one
    // by luck.
    let sessions = vec![
        started("aaa", "Thu Sep 17 21:30:00 2026", 9_000),
        started("bbb", "Thu Sep 17 21:29:00 2026", 1_000),
    ];
    let index = index(sessions);
    let order: Vec<&str> = index
        .by_project()
        .values()
        .next()
        .expect("one project")
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["bbb", "aaa"],
        "the most recently written session climbed to the top"
    );
}

#[test]
fn a_row_keeps_its_place_when_a_session_writes() {
    // The same list, rebuilt after the older session writes again. Nothing may
    // move: this is the whole point of the ordering.
    let before = index(vec![
        started("aaa", "Thu Sep 17 21:30:00 2026", 9_000),
        started("bbb", "Thu Sep 17 21:29:00 2026", 1_000),
    ]);
    let after = index(vec![
        started("aaa", "Thu Sep 17 21:30:00 2026", 9_000),
        // bbb just wrote, and is now the most recent by mtime.
        started("bbb", "Thu Sep 17 21:29:00 2026", 99_000),
    ]);

    let ids = |index: &SessionIndex| -> Vec<String> {
        index
            .by_project()
            .values()
            .next()
            .expect("one project")
            .iter()
            .map(|s| s.session_id.clone())
            .collect()
    };
    assert_eq!(
        ids(&before),
        ids(&after),
        "the list reordered after a write"
    );
}

#[test]
fn rfc3339_start_times_still_work() {
    // The scanner prints `ps` format, but the parser accepts both and nothing
    // should depend on which one a session happens to carry.
    let sessions = vec![
        started("aaa", "2026-09-17T21:30:00Z", 1_000),
        started("bbb", "2026-09-17T21:29:00Z", 9_000),
    ];
    let built = index(sessions);
    let order: Vec<String> = built
        .by_project()
        .values()
        .next()
        .expect("one project")
        .iter()
        .map(|s| s.session_id.clone())
        .collect();
    assert_eq!(order, vec!["bbb".to_string(), "aaa".to_string()]);
}

#[test]
fn a_session_with_no_start_time_still_sorts_somewhere_stable() {
    // A transcript with no live process has no lstart at all. It falls back to
    // the mtime, which is the best available, and must not panic or vanish.
    let mut orphan = a_session("ccc", "/repo/alpha");
    orphan.lstart = None;
    orphan.session_mtime = UNIX_EPOCH + Duration::from_secs(5_000);

    let index = index(vec![
        started("aaa", "Thu Sep 17 21:30:00 2026", 9_000),
        orphan,
    ]);
    assert_eq!(
        index
            .by_project()
            .values()
            .next()
            .expect("one project")
            .len(),
        2
    );
}
