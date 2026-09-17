//! Sessions read out of the transcript store, which is what a machine with no
//! readable process table lists instead of nothing at all.
//!
//! Driven by [`Discovery`] rather than by `cfg`, so the layer native Windows
//! runs is exercised by the ordinary test run on every platform.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use crate::paths::Paths;
use crate::scan::{Discovery, Scanner, UnsupportedProcessSource, ACTIVITY_WINDOW};
use crate::types::RawSession;

const MINUTE: Duration = Duration::from_secs(60);
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A throwaway transcript store, written to directly — there is no process
/// anywhere in this file, which is the point.
struct Store {
    _dir: TempDir,
    paths: Paths,
}

impl Store {
    fn new() -> Store {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_test(dir.path());
        fs::create_dir_all(&paths.projects_dir).expect("mkdir projects");
        Store { _dir: dir, paths }
    }

    /// A transcript in the project directory Claude Code would use for `cwd`,
    /// last written `age` ago.
    fn transcript(&self, cwd: &str, name: &str, age: Duration) -> PathBuf {
        let dir = self.paths.project_transcripts(cwd);
        self.write_in(&dir, name, Some(cwd), age)
    }

    /// The same, in a directory named whatever the caller says — the Windows
    /// case, where the encoding is not known and must not be assumed.
    fn transcript_in(&self, dir_name: &str, cwd: Option<&str>, name: &str) -> PathBuf {
        let dir = self.paths.projects_dir.join(dir_name);
        self.write_in(&dir, name, cwd, Duration::ZERO)
    }

    fn write_in(&self, dir: &Path, name: &str, cwd: Option<&str>, age: Duration) -> PathBuf {
        fs::create_dir_all(dir).expect("mkdir");
        let path = dir.join(format!("{name}.jsonl"));
        let body = match cwd {
            Some(cwd) => {
                serde_json::json!({
                    "type": "user",
                    "sessionId": name,
                    "cwd": cwd,
                    "message": { "role": "user", "content": "hello" },
                })
                .to_string()
                    + "\n"
            }
            // A session that has started but not flushed its first entry.
            None => String::new(),
        };
        fs::write(&path, body).expect("write");
        fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(SystemTime::now() - age)
            .expect("set mtime");
        path
    }

    fn scanner(&self, discovery: Discovery) -> Scanner<UnsupportedProcessSource> {
        Scanner::new(UnsupportedProcessSource, self.paths.clone(), discovery)
    }

    fn scan(&self) -> Vec<RawSession> {
        self.scanner(Discovery::Transcripts)
            .scan_sessions(SystemTime::now())
    }
}

fn ids(sessions: &[RawSession]) -> Vec<&str> {
    sessions.iter().map(|s| s.session_id.as_str()).collect()
}

#[test]
fn a_recently_written_transcript_is_a_session_with_no_process() {
    let store = Store::new();
    let file = store.transcript("/repo/app", "alpha", Duration::ZERO);

    let sessions = store.scan();
    assert_eq!(ids(&sessions), vec!["alpha"]);
    let session = &sessions[0];
    assert_eq!(session.session_file.as_deref(), Some(file.as_path()));
    assert_eq!(session.cwd, "/repo/app");
    assert!(session.session_size.unwrap() > 0);
    // Everything that needs a process is absent rather than invented: no pid to
    // signal, no tty to join, no start time to render.
    assert!(session.pids.is_empty());
    assert_eq!(session.tty, None);
    assert_eq!(session.lstart, None);
    // And no status either — the transcript decides that downstream, exactly as
    // it does for a paired session.
    assert_eq!(session.status, None);
    assert!(!session.starting);
}

#[test]
fn a_transcript_nobody_has_touched_for_a_week_is_not_a_session() {
    // The store is a history, not a list of what is running. Listing all of it
    // would put months of finished work in the live sessions tab — the same
    // wrong as showing a week-old session as live on somebody else's terminal
    // tab, which is what the `starting-<pid>` rule exists to stop.
    let store = Store::new();
    store.transcript("/repo/app", "alpha", Duration::ZERO);
    store.transcript("/repo/app", "last-week", WEEK);
    store.transcript("/repo/other", "last-week-too", WEEK);

    assert_eq!(ids(&store.scan()), vec!["alpha"]);
}

#[test]
fn the_window_is_the_one_the_constant_names() {
    let store = Store::new();
    store.transcript("/repo/app", "inside", ACTIVITY_WINDOW - MINUTE);
    store.transcript("/repo/app", "outside", ACTIVITY_WINDOW + MINUTE);

    assert_eq!(ids(&store.scan()), vec!["inside"]);
}

#[test]
fn the_working_directory_comes_from_the_transcript_not_from_its_folder_name() {
    // The folder name is a lossy encoding — every separator became a dash and
    // the dashes already in the path were left alone — so it cannot be decoded
    // back, and on Windows its exact spelling is not even known. The file says
    // where it ran; nothing else has to.
    let store = Store::new();
    store.transcript_in("C--Users-jane-app", Some(r"C:\Users\jane\app"), "alpha");

    let sessions = store.scan();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].cwd, r"C:\Users\jane\app");
}

#[test]
fn a_transcript_with_nothing_written_in_it_yet_is_listed_without_a_directory() {
    // A session that has started but not flushed its first entry. Hiding it
    // would make a launch look like it did nothing; an empty directory is what
    // the row honestly knows, and the project label reads "unknown".
    let store = Store::new();
    store.transcript_in("-repo-app", None, "just-born");

    let sessions = store.scan();
    assert_eq!(ids(&sessions), vec!["just-born"]);
    assert_eq!(sessions[0].cwd, "");
}

#[test]
fn a_prewarmed_worker_is_not_a_session_here_either() {
    // Claude Code's own scratch workers write transcripts like anything else.
    // With a process to inspect they are filtered by their cwd; without one,
    // the transcript reports the same cwd and the same rule applies.
    let store = Store::new();
    store.transcript(
        "/tmp/cc-daemon-501/f733b519/spare",
        "worker",
        Duration::ZERO,
    );
    store.transcript("/repo/app", "alpha", Duration::ZERO);

    assert_eq!(ids(&store.scan()), vec!["alpha"]);
}

#[test]
fn subagent_side_files_are_not_sessions() {
    let store = Store::new();
    store.transcript("/repo/app", "agent-helper", Duration::ZERO);
    store.transcript("/repo/app", "agent-acompact-1", Duration::ZERO);
    store.transcript("/repo/app", "alpha", Duration::ZERO);

    assert_eq!(ids(&store.scan()), vec!["alpha"]);
}

#[test]
fn the_newest_write_is_listed_first() {
    let store = Store::new();
    store.transcript("/repo/app", "older", 30 * MINUTE);
    store.transcript("/repo/other", "newest", Duration::ZERO);
    store.transcript("/repo/app", "middle", 5 * MINUTE);

    assert_eq!(ids(&store.scan()), vec!["newest", "middle", "older"]);
}

#[test]
fn a_second_pass_reads_no_directory_and_no_head_again() {
    // The listing is cached on its directory's mtime and the working directory
    // on the file, so a steady tick costs one stat per transcript — see the
    // cost note on `transcript_sessions`.
    let store = Store::new();
    store.transcript("/repo/app", "alpha", Duration::ZERO);
    store.transcript("/repo/other", "beta", Duration::ZERO);
    let mut scanner = store.scanner(Discovery::Transcripts);

    for _ in 0..3 {
        assert_eq!(scanner.scan_sessions(SystemTime::now()).len(), 2);
    }
    assert_eq!(scanner.session_files().builds(), 2, "directories re-read");
    assert_eq!(scanner.session_cwds().reads(), 2, "heads re-read");
}

#[test]
fn the_layer_is_off_wherever_the_process_table_can_be_read() {
    // Unix keeps its semantics exactly: a transcript with no process behind it
    // is not a session, however recently it was written.
    let store = Store::new();
    store.transcript("/repo/app", "alpha", Duration::ZERO);

    let sessions = store
        .scanner(Discovery::Processes)
        .scan_sessions(SystemTime::now());
    assert!(sessions.is_empty(), "{:?}", ids(&sessions));
}

#[test]
fn the_platform_picks_the_layer_from_its_discovery_flag() {
    let expected = if crate::platform::LIVE_DISCOVERY {
        Discovery::Processes
    } else {
        Discovery::Transcripts
    };
    assert_eq!(Discovery::for_platform(), expected);
}
