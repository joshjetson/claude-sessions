//! Covers the tag map and the run-log discovery. The Node app had no test file
//! for `autodev.js`; these pin the two behaviours the board depends on — that a
//! blocked state beats a progress one, and that a machine with no daemon gets
//! an empty list rather than an error.

use super::*;

fn tags(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_string()).collect()
}

#[test]
fn no_tags_is_no_state() {
    assert!(auto_dev_state(&[]).is_none());
    assert!(auto_dev_state(&tags(&["bug", "customer"])).is_none());
    assert!(auto_dev_trail(&[]).is_empty());
}

#[test]
fn the_first_matching_tag_in_pipeline_order_wins() {
    let state = auto_dev_state(&tags(&["auto_sized", "auto_implemented"])).unwrap();
    assert_eq!(state.tag, "auto_implemented");
    assert_eq!(state.kind, AutoKind::Running);
}

#[test]
fn blocked_and_paused_states_override_progress() {
    // A task that got as far as an MR and then asked for help needs help; the
    // board must not read it as "implemented".
    let blocked = auto_dev_state(&tags(&["auto_implemented", "needs_intervention"])).unwrap();
    assert_eq!(blocked.tag, "needs_intervention");
    assert_eq!(blocked.kind, AutoKind::Blocked);

    let paused = auto_dev_state(&tags(&["auto_qa_submitted", "auto_paused"])).unwrap();
    assert_eq!(paused.tag, "auto_paused");
    assert_eq!(paused.kind, AutoKind::Paused);
}

#[test]
fn all_twelve_states_are_present_and_uniquely_tagged() {
    assert_eq!(STATES.len(), 12);
    let unique: std::collections::HashSet<&str> = STATES.iter().map(|s| s.tag).collect();
    assert_eq!(unique.len(), STATES.len());
    assert!(is_auto_tag("auto_qa_pass"));
    assert!(!is_auto_tag("qa_pass"));
}

#[test]
fn the_trail_lists_every_tag_in_pipeline_order() {
    let trail = auto_dev_trail(&tags(&["auto_qa_submitted", "auto_sized", "auto_review"]));
    assert_eq!(
        trail.iter().map(|s| s.tag).collect::<Vec<_>>(),
        vec!["auto_qa_submitted", "auto_review", "auto_sized"]
    );
}

#[test]
fn the_board_marker_carries_the_glyph_and_its_colour() {
    let marker = board_marker(&tags(&["auto_qa_fail"])).unwrap();
    assert_eq!(marker.marker, "🤖✗ qa");
    assert_eq!(marker.color, crate::types::Color::Red);
    assert!(board_marker(&tags(&["nothing"])).is_none());
}

// --- run logs ---------------------------------------------------------------

#[test]
fn a_machine_without_the_daemon_lists_nothing() {
    let missing = std::path::Path::new("/nonexistent/auto-dev-daemon/runs");
    assert!(list_run_logs(missing, 6440).is_empty());
    assert!(!has_run_logs(missing, 6440));
}

#[test]
fn logs_are_matched_by_task_id_and_returned_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for name in [
        "implement-6440-20260901T100000.log",
        "review-6440-20260901T090000.log",
        "implement-6441-20260901T110000.log",
        "notes-6440.txt",
    ] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    // Make the ordering explicit rather than trusting write order.
    let newer = std::time::SystemTime::now();
    let older = newer - std::time::Duration::from_secs(3600);
    set_mtime(&dir.join("implement-6440-20260901T100000.log"), newer);
    set_mtime(&dir.join("review-6440-20260901T090000.log"), older);

    let logs = list_run_logs(dir, 6440);
    assert_eq!(logs.len(), 2, "only this task's .log files");
    assert_eq!(logs[0].action, "implement");
    assert_eq!(logs[1].action, "review");
    assert!(has_run_logs(dir, 6440));
    assert!(!has_run_logs(dir, 9999));
}

#[test]
fn the_action_is_everything_before_the_task_id() {
    assert_eq!(action_of("qa-dry-run-6440-2026.log"), "qa-dry-run");
    assert_eq!(action_of("implement-6440-x.log"), "implement");
    // Nothing that looks like `-<digits>-` leaves the name alone.
    assert_eq!(action_of("orphan.log"), "orphan.log");
}

/// `filetime` is not a dependency; a fresh write plus an explicit utimes via
/// the standard library's `File::set_times` keeps the test deterministic.
fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}
