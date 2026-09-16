//! Pairing by the task a session was launched for.
//!
//! The 2026-09-15 incident. Eight tasks were started from one folder inside 20
//! seconds. Each transcript is born about 1.7s after its OWN process, but only
//! about 0.3s before the NEXT one — and the closest-pair rule took the smaller
//! gap. Every process got its successor's transcript, so seven of eight
//! sessions showed the wrong task, the wrong tty and the wrong terminal tab.
//!
//! Nothing about the timing can separate these. The launch states its task in
//! `CLAUDE_SESSIONS_TASK_ID`, and the transcript states the same task in its
//! opening prompt, so the two are matched on that instead.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::transcript::TaskRefCache;
use crate::types::SessionFile;

use super::{a_file, a_proc, at, pair, pair_with, ClaudeProcess};

/// A transcript on disk whose opening prompt names `task_id`.
fn task_file(dir: &TempDir, name: &str, task_id: i64, born_ms: u64) -> SessionFile {
    let path: PathBuf = dir.path().join(format!("{name}.jsonl"));
    let body = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": format!(
                "Run /qa. https://odoo/web#id={task_id}&model=project.task&view_type=form"
            ),
        },
    });
    std::fs::write(&path, format!("{body}\n")).expect("write transcript");
    SessionFile {
        name: format!("{name}.jsonl"),
        path,
        mtime: at(born_ms),
        size: 10,
        birthtime: Some(at(born_ms)),
    }
}

/// The measured timings, as millisecond offsets from the first launch.
const LAUNCHES: [(i64, u64, u64); 8] = [
    (6137, 0, 1873),
    (6661, 7000, 8708),
    (6660, 9000, 11413),
    (6659, 11000, 12986),
    (6277, 12000, 14530),
    (6272, 15000, 16804),
    (6270, 16000, 19509),
    (6273, 20000, 21948),
];

#[test]
fn eight_launches_two_seconds_apart_each_keep_their_own_transcript() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut procs: Vec<ClaudeProcess> = LAUNCHES
        .iter()
        .enumerate()
        .map(|(i, (task, start, _))| ClaudeProcess {
            tty: Some(format!("ttys0{i}")),
            launch_task_id: Some(*task),
            ..a_proc(*task as u32, *start)
        })
        .collect();
    procs.reverse(); // newest-started first
    let mut files: Vec<SessionFile> = LAUNCHES
        .iter()
        .map(|(task, _, born)| task_file(&dir, &format!("launch-{task}"), *task, *born))
        .collect();
    files.reverse(); // newest mtime first

    let mut cache = TaskRefCache::new();
    let paired = pair_with(&procs, &files, &mut |path| cache.get(path));

    for (task, _, _) in LAUNCHES {
        let got = paired.iter().find(|(pid, _)| *pid == task as u32);
        assert_eq!(
            got.map(|(_, name)| name.as_str()),
            Some(format!("launch-{task}").as_str()),
            "task {task} was paired with another task's transcript"
        );
    }
    // Every file was read at most once, however many candidates it appeared in.
    assert!(
        cache.reads() <= LAUNCHES.len() as u64,
        "read {} files for {} transcripts",
        cache.reads(),
        LAUNCHES.len()
    );
}

#[test]
fn a_transcript_naming_another_task_is_never_taken() {
    let dir = tempfile::tempdir().expect("temp dir");
    let procs = [ClaudeProcess {
        launch_task_id: Some(6270),
        ..a_proc(1, 1000)
    }];
    let files = [task_file(&dir, "someone-else", 6272, 1500)];
    let mut cache = TaskRefCache::new();
    assert!(
        pair_with(&procs, &files, &mut |p| cache.get(p)).is_empty(),
        "paired a process with another task's transcript"
    );
}

#[test]
fn the_command_line_session_id_still_wins_over_the_launch_task_id() {
    // --resume names the exact file. Nothing may override that.
    let dir = tempfile::tempdir().expect("temp dir");
    let procs = [ClaudeProcess {
        session_id: Some("exact".to_string()),
        launch_task_id: Some(6270),
        ..a_proc(1, 1000)
    }];
    let files = [
        a_file("exact", 2_000, Some(500)),
        task_file(&dir, "by-task", 6270, 1500),
    ];
    let mut cache = TaskRefCache::new();
    assert_eq!(
        pair_with(&procs, &files, &mut |p| cache.get(p)),
        vec![(1, "exact".to_string())]
    );
}

#[test]
fn a_session_with_no_launch_task_id_still_pairs_on_birth_time() {
    let procs = [a_proc(1, 1000)];
    let files = [a_file("fresh", 2_000, Some(2000))];
    assert_eq!(pair(&procs, &files), vec![(1, "fresh".to_string())]);
}

#[test]
fn a_transcript_born_before_its_process_is_not_attributed_to_it() {
    // The old 2000ms slack let a file be claimed by a process that did not yet
    // exist when the file was created — which is what shifted the chain.
    let procs = [a_proc(1, 10_000)];
    let files = [a_file("earlier", 2_000, Some(8_500))];
    assert!(pair(&procs, &files).is_empty());
}

#[test]
fn a_transcript_born_within_the_slack_is_still_this_processs() {
    // `ps` truncates a start time to the second, so a transcript can look a few
    // hundred milliseconds older than the process that wrote it.
    let procs = [a_proc(1, 10_000)];
    let files = [a_file("just-before", 2_000, Some(9_700))];
    assert_eq!(pair(&procs, &files), vec![(1, "just-before".to_string())]);
}

#[test]
fn a_transcript_from_beyond_the_window_is_not_this_processs() {
    let procs = [a_proc(1, 0)];
    let files = [a_file("much-later", 2_000, Some(15 * 60 * 1000 + 1))];
    assert!(pair(&procs, &files).is_empty());
}

#[test]
fn a_task_on_its_second_round_takes_the_current_transcript() {
    // Files arrive newest-first, so the round-two transcript is found first and
    // the round-one one is left for the archive.
    let dir = tempfile::tempdir().expect("temp dir");
    let procs = [ClaudeProcess {
        launch_task_id: Some(6137),
        ..a_proc(1, 50_000)
    }];
    let files = [
        task_file(&dir, "round-two", 6137, 60_000),
        task_file(&dir, "round-one", 6137, 10),
    ];
    let mut cache = TaskRefCache::new();
    assert_eq!(
        pair_with(&procs, &files, &mut |p| cache.get(p)),
        vec![(1, "round-two".to_string())]
    );
}

#[test]
fn a_launch_task_id_pairs_even_when_the_transcript_predates_the_process() {
    // Identity beats timing: a resumed task whose transcript is days old is
    // still that task's, which is exactly what birth time cannot say.
    let dir = tempfile::tempdir().expect("temp dir");
    let procs = [ClaudeProcess {
        launch_task_id: Some(4033),
        ..a_proc(1, 900_000)
    }];
    let files = [task_file(&dir, "older", 4033, 10)];
    let mut cache = TaskRefCache::new();
    assert_eq!(
        pair_with(&procs, &files, &mut |p| cache.get(p)),
        vec![(1, "older".to_string())]
    );
}
