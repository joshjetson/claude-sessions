//! A whole tick, against a scripted process table.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use tempfile::TempDir;

use crate::paths::Paths;
use crate::scan::Scanner;
use crate::types::{RawSession, SessionStatus};

use super::FakeProcesses;

const CWD: &str = "/Users/k/dev/app";
const OTHER_CWD: &str = "/Users/k/dev/other";
const UUID: &str = "0198e4f0-1b3c-7a2d-9f4e-5c6b7a8d9e0f";

struct Env {
    _dir: TempDir,
    paths: Paths,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = Paths::for_test(dir.path());
        Env { _dir: dir, paths }
    }

    /// A transcript in the project directory Claude Code would use for `cwd`.
    fn transcript(&self, cwd: &str, name: &str) -> std::path::PathBuf {
        let dir = self.paths.project_transcripts(cwd);
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(format!("{name}.jsonl"));
        fs::write(&path, "{\"type\":\"user\"}\n").expect("write");
        path
    }

    fn scanner(&self, procs: FakeProcesses) -> Scanner<FakeProcesses> {
        Scanner::new(procs, self.paths.clone())
    }
}

fn ids(sessions: &[RawSession]) -> Vec<&str> {
    sessions.iter().map(|s| s.session_id.as_str()).collect()
}

#[test]
fn a_declared_session_becomes_a_row_carrying_its_process_details() {
    let env = Env::new();
    let file = env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(501, "claude", Some(CWD), 0);
    procs.with_argv(501, &format!("claude --resume {UUID}"));

    let sessions = env.scanner(procs).scan_sessions(SystemTime::now());
    assert_eq!(ids(&sessions), vec![UUID]);
    let session = &sessions[0];
    assert_eq!(session.pids, vec![501]);
    assert_eq!(session.cwd, CWD);
    assert_eq!(session.tty.as_deref(), Some("ttys501"));
    assert_eq!(session.session_file.as_deref(), Some(file.as_path()));
    assert!(session.session_size.unwrap() > 0);
    assert!(session.lstart.is_some());
    assert!(!session.starting);
    assert_eq!(session.status, None);
}

#[test]
fn a_process_with_no_transcript_yet_is_a_starting_placeholder() {
    // Freshly spawned, or still at the trust prompt. It is surfaced so a launch
    // is visible immediately, and it is never given somebody else's file.
    let env = Env::new();
    env.transcript(CWD, "someone-elses");
    let procs = FakeProcesses::new();
    procs.add(777, "claude", Some(CWD), 0);

    let now = SystemTime::now();
    let sessions = env.scanner(procs).scan_sessions(now);
    assert_eq!(ids(&sessions), vec!["starting-777"]);
    assert_eq!(sessions[0].session_file, None);
    assert_eq!(sessions[0].status, Some(SessionStatus::Starting));
    assert!(sessions[0].starting);
    assert_eq!(sessions[0].session_mtime, now);
}

#[test]
fn several_processes_on_one_transcript_become_one_row_with_several_pids() {
    let env = Env::new();
    env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    procs.with_argv(1, &format!("claude --resume {UUID}"));
    procs.add(2, "claude", Some(CWD), 30);
    procs.with_argv(2, &format!("claude --resume {UUID}"));

    let sessions = env.scanner(procs).scan_sessions(SystemTime::now());
    assert_eq!(sessions.len(), 1);
    // Newest process first, because it keeps the session's tty.
    assert_eq!(sessions[0].pids, vec![2, 1]);
    assert_eq!(sessions[0].tty.as_deref(), Some("ttys002"));
}

#[test]
fn helpers_scratch_directories_and_unreadable_cwds_never_become_sessions() {
    let env = Env::new();
    env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    // Caught by `ps -o comm`.
    procs.add(10, "claude bg-spare", Some(CWD), 0);
    // Caught only by the full command line.
    procs.add(11, "claude", Some(CWD), 0);
    procs.with_argv(11, "claude --bg-pty-host");
    // Prewarm scratch area.
    procs.add(12, "claude", Some("/private/tmp/cc-daemon-501/ab/spare"), 0);
    // lsof said nothing.
    procs.add(13, "claude", None, 0);
    // Not a session at all.
    procs.add(14, "node", Some(CWD), 0);

    assert!(env
        .scanner(procs)
        .scan_sessions(SystemTime::now())
        .is_empty());
}

#[test]
fn sessions_are_grouped_by_the_project_directory_their_cwd_encodes_to() {
    let env = Env::new();
    env.transcript(CWD, UUID);
    let other = "1111e4f0-1b3c-7a2d-9f4e-5c6b7a8d9e0f";
    env.transcript(OTHER_CWD, other);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    procs.with_argv(1, &format!("claude --resume {UUID}"));
    procs.add(2, "claude", Some(OTHER_CWD), 0);
    procs.with_argv(2, &format!("claude --resume {other}"));

    let sessions = env.scanner(procs).scan_sessions(SystemTime::now());
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].cwd, CWD);
    assert_eq!(sessions[1].cwd, OTHER_CWD);
    // A transcript only ever belongs to its own project's directory.
    assert!(sessions[0]
        .session_file
        .as_deref()
        .unwrap()
        .starts_with(env.paths.project_transcripts(CWD)));
}

#[test]
fn the_launch_task_is_read_from_the_environment_and_pairs_the_transcript() {
    let env = Env::new();
    let dir = env.paths.project_transcripts(CWD);
    fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("by-task.jsonl");
    fs::write(
        &path,
        "{\"message\":{\"content\":\"see https://x/web#id=6137&model=project.task&view_type=form\"}}\n",
    )
    .expect("write");

    let procs = FakeProcesses::new();
    procs.add(42, "claude", Some(CWD), 0);
    procs.with_environ(42, "claude CLAUDE_SESSIONS_TASK_ID=6137 TERM=xterm");

    let sessions = env.scanner(procs).scan_sessions(SystemTime::now());
    assert_eq!(ids(&sessions), vec!["by-task"]);
    assert_eq!(sessions[0].session_file.as_deref(), Some(path.as_path()));
}

#[test]
fn a_transcript_naming_another_task_is_refused_even_end_to_end() {
    let env = Env::new();
    let dir = env.paths.project_transcripts(CWD);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join("theirs.jsonl"),
        "{\"message\":{\"content\":\"https://x/web#id=6272&model=project.task\"}}\n",
    )
    .expect("write");

    let procs = FakeProcesses::new();
    procs.add(42, "claude", Some(CWD), 0);
    procs.with_environ(42, "claude CLAUDE_SESSIONS_TASK_ID=6270");

    let sessions = env.scanner(procs).scan_sessions(SystemTime::now());
    assert_eq!(ids(&sessions), vec!["starting-42"]);
}

#[test]
fn a_live_transcripts_mtime_is_fresh_even_when_the_directory_has_not_changed() {
    // The cached listing is rebuilt only when the directory changes, and
    // appending to a transcript does not change that — but the status machine
    // reads this mtime, so the paired files are re-stat'd.
    let env = Env::new();
    let path = env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    procs.with_argv(1, &format!("claude --resume {UUID}"));
    let mut scanner = env.scanner(procs);

    let first = scanner.scan_sessions(SystemTime::now());
    append(&path, "{\"type\":\"assistant\"}\n");
    let second = scanner.scan_sessions(SystemTime::now());

    assert!(
        second[0].session_mtime >= first[0].session_mtime,
        "a stale mtime would make a working session look idle"
    );
    assert!(second[0].session_size.unwrap() > first[0].session_size.unwrap());
    assert_eq!(
        scanner.session_files().builds(),
        1,
        "the directory was re-read"
    );
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open");
    file.write_all(text.as_bytes()).expect("append");
}

#[test]
fn a_pid_is_only_ever_asked_about_once() {
    // An lsof per process per tick was most of the cost of a scan.
    let env = Env::new();
    env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    procs.with_argv(1, &format!("claude --resume {UUID}"));
    let mut scanner = env.scanner(procs.clone());

    for _ in 0..5 {
        scanner.scan_sessions(SystemTime::now());
    }
    let calls = procs.calls();
    assert_eq!(calls.list, 5, "the process listing is read every tick");
    assert_eq!(calls.cwd, 1);
    assert_eq!(calls.argv, 1);
    assert_eq!(calls.environ, 1);
}

#[test]
fn the_pid_caches_are_pruned_to_the_processes_that_are_still_alive() {
    let env = Env::new();
    env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    let mut scanner = env.scanner(procs.clone());
    scanner.scan_sessions(SystemTime::now());
    assert_eq!(procs.calls().cwd, 1);

    // The process exits and a new one reuses the number, as pids do.
    procs.remove(1);
    scanner.scan_sessions(SystemTime::now());
    procs.add(1, "claude", Some(OTHER_CWD), 60);
    let sessions = scanner.scan_sessions(SystemTime::now());

    assert_eq!(procs.calls().cwd, 2, "a recycled pid kept the old answer");
    assert_eq!(sessions[0].cwd, OTHER_CWD);
}

#[test]
fn a_process_that_ps_forgets_between_the_two_calls_still_gets_an_answer() {
    // Otherwise it is re-queried on every tick for as long as it is listed.
    let env = Env::new();
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    procs.forget_details(1);
    let mut scanner = env.scanner(procs.clone());

    scanner.scan_sessions(SystemTime::now());
    scanner.scan_sessions(SystemTime::now());
    assert_eq!(procs.calls().argv, 1);
    assert_eq!(procs.calls().environ, 1);
}

#[test]
fn a_project_nobody_is_working_in_is_forgotten() {
    let env = Env::new();
    env.transcript(CWD, UUID);
    let procs = FakeProcesses::new();
    procs.add(1, "claude", Some(CWD), 0);
    let mut scanner = env.scanner(procs.clone());
    scanner.scan_sessions(SystemTime::now());
    assert_eq!(scanner.session_files().builds(), 1);

    procs.remove(1);
    scanner.scan_sessions(SystemTime::now());
    procs.add(2, "claude", Some(CWD), 30);
    scanner.scan_sessions(SystemTime::now());
    assert_eq!(scanner.session_files().builds(), 2);
}
