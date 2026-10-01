//! Reading past QA passes out of the transcripts.

use std::io::Write;

use serde_json::json;

use crate::qarun::backfill::{backfill, parked_verdict, read_pass};
use crate::qarun::outcomes::OutcomeLog;

#[test]
fn the_command_that_parks_a_verdict_names_the_task_and_the_verdict() {
    assert_eq!(
        parked_verdict(
            r#"claude-sessions notify --title "QA #6391: REVISION REQUIRED" --message "x" --level warn"#
        ),
        Some((6391, "revisions".to_string()))
    );
    assert_eq!(
        parked_verdict(r#"/x/claude-sessions notify --title "QA #7001: PASS""#),
        Some((7001, "pass".to_string()))
    );
    assert_eq!(
        parked_verdict(r#"notify --title "QA #7001: CHECKPOINT""#),
        None
    );
    assert_eq!(parked_verdict(r#"echo "QA #7001: PASS""#), None);
}

/// A QA session's transcript: started at 10:00, three turns of usage, the
/// verdict parked at 10:40, then more after it that must not count.
fn transcript(dir: &std::path::Path, folder: &str, name: &str, task: i64, cwd: &str) {
    let folder = dir.join(folder);
    std::fs::create_dir_all(&folder).unwrap();
    let mut file = std::fs::File::create(folder.join(format!("{name}.jsonl"))).unwrap();
    let usage = |id: &str, n: u64| {
        json!({
            "type": "assistant",
            "timestamp": "2026-09-30T10:20:00Z",
            "message": { "id": id, "usage": {
                "input_tokens": n, "cache_creation_input_tokens": n,
                "cache_read_input_tokens": 1_000_000, "output_tokens": n,
            }, "content": [] },
        })
    };
    let lines = vec![
        json!({ "type": "user", "timestamp": "2026-09-30T10:00:00Z", "cwd": cwd,
                "message": { "content": "/qa" } }),
        usage("m1", 1000),
        usage("m1", 1000),
        usage("m2", 2000),
        json!({ "type": "assistant", "timestamp": "2026-09-30T10:40:00Z",
                "message": { "id": "m3", "content": [{ "type": "tool_use", "name": "Bash",
                "input": { "command": format!("claude-sessions notify --title \"QA #{task}: PASS\"") } }] } }),
        usage("m9", 50_000),
    ];
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

#[test]
fn a_pass_is_timed_to_its_verdict_and_counts_new_tokens_once() {
    let dir = tempfile::tempdir().unwrap();
    transcript(dir.path(), "-repo-aurora", "s1", 6391, "/repo/aurora");
    let pass = read_pass(&dir.path().join("-repo-aurora/s1.jsonl")).expect("a pass");
    assert_eq!(pass.task_id, 6391);
    assert_eq!(pass.verdict, "pass");
    assert_eq!(pass.minutes, 40.0);
    // m1 counted once, m2 once, the line after the verdict not at all, and
    // no cache reads.
    assert_eq!(pass.tokens, 3 * 1000 + 3 * 2000);
    assert_eq!(pass.cwd, "/repo/aurora");
}

#[test]
fn a_backfill_adds_what_it_can_map_once_and_skips_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    transcript(&projects, "-repo-aurora", "s1", 6391, "/repo/aurora");
    transcript(&projects, "-repo-elsewhere", "s2", 6392, "/repo/elsewhere");
    let runtime = dir.path().join("runtime");
    let log = OutcomeLog::new(&runtime);
    let project_of = |cwd: &str| (cwd == "/repo/aurora").then(|| "Aurora".to_string());

    let report = backfill(&projects, &log, project_of);
    assert_eq!(report.transcripts, 2);
    assert_eq!(report.passes, 2);
    assert_eq!(report.added, 1);
    assert_eq!(report.no_project, 1);
    assert_eq!(log.read()[0].project, "Aurora");

    let again = backfill(&projects, &log, project_of);
    assert_eq!(again.added, 0, "a second backfill added a pass twice");
    assert_eq!(log.read().len(), 1);
}
