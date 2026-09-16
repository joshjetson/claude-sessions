//! The task a transcript's opening prompt names.
//!
//! Node read this in two places with two different cache rules (`scanner.js`
//! and `archive.js`); here it is one function, and these are the rules both
//! callers get.

use std::fs;

use crate::transcript::{first_task_ref, task_ref_in, TaskRefCache, TASK_REF_HEAD_BYTES};

const REF: &str = "https://odoo.example/web#id=6137&model=project.task&view_type=form";

#[test]
fn a_task_link_in_a_spawn_prompt_names_the_task() {
    assert_eq!(task_ref_in(REF), Some(6137));
    assert_eq!(
        task_ref_in("look at #id=4033&model=project.task please"),
        Some(4033)
    );
}

#[test]
fn a_link_to_something_else_is_not_a_task() {
    assert_eq!(task_ref_in("id=12&model=project.project"), None);
    assert_eq!(task_ref_in("model=project.task&id=12"), None);
    assert_eq!(task_ref_in("id=&model=project.task"), None);
    assert_eq!(task_ref_in("nothing here"), None);
}

#[test]
fn the_first_reference_wins() {
    // The spawn prompt comes first; a later mention of another task in the
    // conversation cannot rename the session.
    let text = format!("{REF} and later id=9999&model=project.task");
    assert_eq!(task_ref_in(&text), Some(6137));
}

#[test]
fn the_reference_is_read_out_of_a_transcript_head() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    fs::write(&path, format!("{{\"message\":\"{REF}\"}}\n")).expect("write");
    assert_eq!(first_task_ref(&path), Some(6137));
}

#[test]
fn a_reference_past_the_head_is_not_found() {
    // A bounded read is the point: the alternative is reading whole multi-
    // megabyte transcripts on every scan tick.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    let padding = "x".repeat(TASK_REF_HEAD_BYTES as usize);
    fs::write(&path, format!("{padding}{REF}")).expect("write");
    assert_eq!(first_task_ref(&path), None);
}

#[test]
fn a_missing_file_has_no_task_rather_than_failing_the_tick() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert_eq!(first_task_ref(&dir.path().join("gone.jsonl")), None);
}

#[test]
fn a_hit_is_read_once_and_remembered_for_good() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    fs::write(&path, format!("{{\"message\":\"{REF}\"}}\n")).expect("write");

    let mut cache = TaskRefCache::new();
    for _ in 0..10 {
        assert_eq!(cache.get(&path), Some(6137));
    }
    assert_eq!(cache.reads(), 1);

    // A transcript's opening prompt never changes, so the remembered answer
    // stands even if the file is rewritten underneath.
    fs::write(&path, "{}\n").expect("rewrite");
    assert_eq!(cache.get(&path), Some(6137));
}

#[test]
fn a_miss_is_never_cached_because_the_prompt_may_not_be_on_disk_yet() {
    // A session that has started but not yet flushed its first prompt has no
    // reference to find. Remembering that would make the file look task-less
    // for as long as the daemon runs.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "{}\n").expect("write");

    let mut cache = TaskRefCache::new();
    assert_eq!(cache.get(&path), None);
    assert_eq!(cache.get(&path), None);
    assert_eq!(cache.reads(), 2, "a miss was cached");

    fs::write(&path, format!("{{\"message\":\"{REF}\"}}\n")).expect("rewrite");
    assert_eq!(cache.get(&path), Some(6137));
}

#[test]
fn pruning_drops_transcripts_that_are_gone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    fs::write(&path, format!("{{\"message\":\"{REF}\"}}\n")).expect("write");

    let mut cache = TaskRefCache::new();
    assert_eq!(cache.get(&path), Some(6137));
    cache.prune(|p| p.exists());
    assert_eq!(cache.reads(), 1);

    fs::remove_file(&path).expect("remove");
    cache.prune(|p| p.exists());
    assert_eq!(cache.get(&path), None);
    assert_eq!(cache.reads(), 2);
}
