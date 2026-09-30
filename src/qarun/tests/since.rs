//! Which QA records belong to the run, and which to an earlier pass.
//!
//! A coordinator started first once reported the verdicts an earlier pass left
//! on disk as its own run's results, before any QA session had begun.

use std::fs;
use std::time::{Duration, SystemTime};

use crate::paths::Paths;
use crate::qarun::{record_line, task_record, Recorded};

fn fixture() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    (dir, paths)
}

fn write_run(paths: &Paths, task_id: i64, verdict: &str) {
    let dir = paths.qa_task_dir(task_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("run.json"),
        format!(r#"{{"verdict": "{verdict}", "meta": {{"head": "abc1234"}}}}"#),
    )
    .unwrap();
}

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn a_record_written_before_the_run_started_is_an_earlier_pass() {
    let (_dir, paths) = fixture();
    write_run(&paths, 6391, "pass");
    let started_later = SystemTime::now() + HOUR;

    let record = task_record(&paths, 6391, started_later);
    assert_eq!(record.recorded, Recorded::Earlier);
    let line = record_line(&record);
    assert!(line.starts_with("#6391 not started in this run"), "{line}");
    assert!(line.contains("NOT this run's result"), "{line}");
    assert!(line.contains("pass"), "{line}");
}

#[test]
fn a_record_written_since_the_run_started_is_this_runs() {
    let (_dir, paths) = fixture();
    write_run(&paths, 6392, "revisions");
    let started_earlier = SystemTime::now() - HOUR;

    let record = task_record(&paths, 6392, started_earlier);
    assert_eq!(record.recorded, Recorded::ThisRun);
    assert_eq!(record.verdict, Some(crate::qaden::QaVerdict::Revisions));
    let line = record_line(&record);
    assert!(
        line.starts_with("#6392 this run: round 1, revisions"),
        "{line}"
    );
}

#[test]
fn a_task_with_no_record_has_not_started() {
    let (_dir, paths) = fixture();
    let record = task_record(&paths, 6393, SystemTime::now());
    assert_eq!(record.recorded, Recorded::Nothing);
    assert_eq!(record_line(&record), "#6393 not started: no QA record yet");
}

/// A file that exists but cannot be parsed is not a pass that never ran.
#[test]
fn an_unreadable_record_is_unknown_rather_than_not_started() {
    let (_dir, paths) = fixture();
    let dir = paths.qa_task_dir(6394);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run.json"), "{ not json").unwrap();

    let record = task_record(&paths, 6394, SystemTime::now() - HOUR);
    assert_eq!(record.recorded, Recorded::Unreadable);
    let line = record_line(&record);
    assert!(line.starts_with("#6394 unknown"), "{line}");
}
