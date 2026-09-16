//! Listing a project's transcripts, and the directory-mtime cache that keeps an
//! unchanged project from costing a readdir and a stat per file every second.

use std::collections::HashSet;
use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use crate::scan::{is_compacting, session_files_in, SessionFilesCache};

/// Write `name` and stamp it, so ordering assertions do not depend on how fast
/// the test machine creates files.
fn write_at(dir: &TempDir, name: &str, mtime: SystemTime) {
    let path = dir.path().join(name);
    fs::write(&path, "{}\n").expect("write");
    set_mtime(&path, mtime);
}

fn set_mtime(path: &Path, mtime: SystemTime) {
    let file = File::options().write(true).open(path).expect("open");
    file.set_times(FileTimes::new().set_modified(mtime))
        .expect("set times");
}

fn ago(seconds: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(seconds)
}

#[test]
fn only_session_transcripts_are_listed_newest_write_first() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "older.jsonl", ago(600));
    write_at(&dir, "newer.jsonl", ago(5));
    // Subagent side-files and anything that is not a transcript.
    write_at(&dir, "agent-abc.jsonl", ago(1));
    write_at(&dir, "agent-acompact-1.jsonl", ago(1));
    write_at(&dir, "notes.md", ago(1));

    let files = session_files_in(dir.path());
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["newer.jsonl", "older.jsonl"]);
    assert!(files[0].size > 0);
    assert!(files[0].path.ends_with("newer.jsonl"));
}

#[test]
fn a_directory_that_does_not_exist_is_no_sessions_not_an_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(session_files_in(&dir.path().join("never-opened")).is_empty());
}

#[test]
fn an_unchanged_directory_is_never_read_twice() {
    // Big-O mandate #2: Node ran a readdir, a stat per file and a sort for
    // every project on every tick.
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "a.jsonl", ago(10));
    let mut cache = SessionFilesCache::new();

    assert_eq!(cache.list(dir.path()).len(), 1);
    assert_eq!(cache.builds(), 1);
    for _ in 0..10 {
        assert_eq!(cache.list(dir.path()).len(), 1);
    }
    assert_eq!(cache.builds(), 1, "an unchanged directory was re-read");
}

#[test]
fn a_new_transcript_invalidates_the_listing() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "a.jsonl", ago(10));
    let mut cache = SessionFilesCache::new();
    assert_eq!(cache.list(dir.path()).len(), 1);

    write_at(&dir, "b.jsonl", ago(1));
    assert_eq!(cache.list(dir.path()).len(), 2);
    assert_eq!(cache.builds(), 2);
}

#[test]
fn a_directory_that_is_still_missing_is_not_read_again() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("not-yet");
    let mut cache = SessionFilesCache::new();
    assert!(cache.list(&missing).is_empty());
    assert!(cache.list(&missing).is_empty());
    assert_eq!(cache.builds(), 1);

    // …but it is picked up as soon as it appears.
    fs::create_dir(&missing).expect("create");
    fs::write(missing.join("a.jsonl"), "{}\n").expect("write");
    assert_eq!(cache.list(&missing).len(), 1);
}

#[test]
fn pruning_forgets_the_projects_a_tick_did_not_look_at() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "a.jsonl", ago(10));
    let mut cache = SessionFilesCache::new();
    cache.list(dir.path());
    assert_eq!(cache.builds(), 1);

    cache.prune(&HashSet::new());
    cache.list(dir.path());
    assert_eq!(cache.builds(), 2, "a pruned directory should be re-read");

    let mut keep = HashSet::new();
    keep.insert(dir.path().to_path_buf());
    cache.prune(&keep);
    cache.list(dir.path());
    assert_eq!(cache.builds(), 2);
}

#[test]
fn a_recent_compaction_sidecar_means_the_session_is_compacting() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "session.jsonl", ago(10));
    write_at(&dir, "agent-acompact-99.jsonl", ago(5));
    let session = dir.path().join("session.jsonl");
    assert!(is_compacting(&session, SystemTime::now()));
}

#[test]
fn an_old_compaction_sidecar_is_just_a_leftover() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "session.jsonl", ago(10));
    write_at(&dir, "agent-acompact-99.jsonl", ago(120));
    let session = dir.path().join("session.jsonl");
    assert!(!is_compacting(&session, SystemTime::now()));
}

#[test]
fn a_session_with_no_sidecars_is_not_compacting() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_at(&dir, "session.jsonl", ago(10));
    write_at(&dir, "agent-other.jsonl", ago(1));
    let session = dir.path().join("session.jsonl");
    assert!(!is_compacting(&session, SystemTime::now()));
    assert!(!is_compacting(
        Path::new("nowhere.jsonl"),
        SystemTime::now()
    ));
}
