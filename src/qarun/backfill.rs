//! Fill the outcome log from QA passes that finished before it existed.
//!
//! The daemon records a pass at its first verdict from now on. Passes before
//! that are still in the transcripts: a QA session's first line is when it
//! started, and its `notify … --title "QA #N: PASS"` (or `REVISION REQUIRED`)
//! is when it parked the verdict. Reading those once means the queue has
//! estimates on its first day instead of after weeks.
//!
//! What it cannot know, it does not guess. A transcript whose folder maps to
//! no Odoo project is skipped, and so is a pass shorter than [`MIN_MINUTES`]
//! (a resumed transcript posting an old verdict) or longer than
//! [`MAX_MINUTES`] (a session left open overnight). Runs twice, it adds
//! nothing the second time: a pass is known by its task and its start.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::DateTime;
use serde_json::Value;

use super::outcomes::{OutcomeLog, QaOutcome};

/// Shorter than this is a resumed transcript posting a verdict it already had.
pub const MIN_MINUTES: f64 = 2.0;
/// Longer than this is a session left open, not a pass that took this long.
pub const MAX_MINUTES: f64 = 8.0 * 60.0;

/// What a backfill did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub transcripts: usize,
    pub passes: usize,
    pub added: usize,
    pub no_project: usize,
    pub out_of_range: usize,
}

/// Read every transcript under `projects_dir` and append the passes the log
/// does not have. `project_of` maps a session's working folder to its Odoo
/// project.
pub fn backfill(
    projects_dir: &Path,
    log: &OutcomeLog,
    project_of: impl Fn(&str) -> Option<String>,
) -> BackfillReport {
    let mut report = BackfillReport::default();
    let mut known: HashSet<(i64, String)> = log
        .read()
        .into_iter()
        .map(|outcome| (outcome.task_id, outcome.started_at))
        .collect();
    let Ok(projects) = fs::read_dir(projects_dir) else {
        return report;
    };
    let mut found: Vec<QaOutcome> = Vec::new();
    for project in projects.flatten() {
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            report.transcripts += 1;
            let Some(pass) = read_pass(&path) else {
                continue;
            };
            report.passes += 1;
            let Some(project) = project_of(&pass.cwd) else {
                report.no_project += 1;
                continue;
            };
            if !(MIN_MINUTES..=MAX_MINUTES).contains(&pass.minutes) {
                report.out_of_range += 1;
                continue;
            }
            if !known.insert((pass.task_id, pass.started_at.clone())) {
                continue;
            }
            found.push(QaOutcome {
                task_id: pass.task_id,
                project,
                round: 0,
                verdict: pass.verdict,
                started_at: pass.started_at,
                finished_at: pass.finished_at,
                minutes: pass.minutes,
                tokens: pass.tokens,
            });
        }
    }
    // Oldest first, so the log reads in time order like one the daemon wrote.
    found.sort_by(|a, b| a.finished_at.cmp(&b.finished_at));
    for outcome in &found {
        log.append(outcome);
    }
    report.added = found.len();
    report
}

/// One pass read off a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct PastPass {
    pub task_id: i64,
    pub verdict: String,
    pub cwd: String,
    pub started_at: String,
    pub finished_at: String,
    pub minutes: f64,
    pub tokens: u64,
}

/// The first verdict a transcript parked, with how long it took and the new
/// tokens spent up to it. `None` when it parked none.
pub fn read_pass(path: &Path) -> Option<PastPass> {
    let reader = BufReader::new(fs::File::open(path).ok()?);
    let mut started: Option<String> = None;
    let mut cwd = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut tokens = 0u64;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let stamp = entry["timestamp"].as_str().map(str::to_string);
        if started.is_none() {
            started.clone_from(&stamp);
        }
        if cwd.is_empty() {
            if let Some(dir) = entry["cwd"].as_str() {
                cwd = dir.to_string();
            }
        }
        let message = &entry["message"];
        if let (Some(usage), Some(id)) = (message.get("usage"), message["id"].as_str()) {
            if seen.insert(id.to_string()) {
                let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                tokens += count("input_tokens")
                    + count("cache_creation_input_tokens")
                    + count("output_tokens");
            }
        }
        let Some(blocks) = message["content"].as_array() else {
            continue;
        };
        for block in blocks {
            if block["type"] != "tool_use" || block["name"] != "Bash" {
                continue;
            }
            let command = block["input"]["command"].as_str().unwrap_or_default();
            let Some((task_id, verdict)) = parked_verdict(command) else {
                continue;
            };
            let (started, finished) = (started?, stamp?);
            let minutes = (DateTime::parse_from_rfc3339(&finished).ok()?
                - DateTime::parse_from_rfc3339(&started).ok()?)
            .num_seconds() as f64
                / 60.0;
            return Some(PastPass {
                task_id,
                verdict,
                cwd,
                started_at: started,
                finished_at: finished,
                minutes,
                tokens,
            });
        }
    }
    None
}

/// The task and verdict in a `claude-sessions notify` command that parks one:
/// `--title "QA #6391: REVISION REQUIRED"`.
pub fn parked_verdict(command: &str) -> Option<(i64, String)> {
    if !command.contains("notify") {
        return None;
    }
    let rest = &command[command.find("QA #")? + 4..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let task_id = digits.parse().ok()?;
    let outcome = rest[digits.len()..].trim_start_matches(':').trim_start();
    let verdict = if outcome.starts_with("PASS") {
        "pass"
    } else if outcome.starts_with("REVISION") {
        "revisions"
    } else {
        return None;
    };
    Some((task_id, verdict.to_string()))
}
