//! How long a QA pass takes, and how much it uses, from the passes before it.
//!
//! Each time a QA session parks the first verdict of a round, the daemon
//! appends one line to `<runtime>/qa-outcomes.jsonl`: which task, when its
//! session started, how long the pass took and how many tokens it used. The
//! queue reads the median of a project's recent passes, so a run header can
//! say what a pass there usually costs.
//!
//! A file rather than a table: the SQLite schema is shared with the Node app,
//! and an outcome log needs nothing a table gives. One line per pass, appended,
//! read whole: a reviewer runs a few dozen passes a week.
//!
//! Tokens are the NEW ones: input, cache writes and output. Cache reads are
//! left out. They run to tens of millions on a long pass and cost a fraction
//! of the rest, so counted in they would drown what the number is for.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file name under the runtime directory.
pub const OUTCOMES_FILE: &str = "qa-outcomes.jsonl";
/// How many recent passes an estimate is read from.
pub const ESTIMATE_WINDOW: usize = 20;
/// Fewer passes than this in a project, and the estimate is taken from every
/// project instead.
pub const MIN_PASSES: usize = 3;

/// One finished QA pass.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QaOutcome {
    pub task_id: i64,
    pub project: String,
    pub round: u32,
    /// `pass` or `revisions`.
    pub verdict: String,
    pub started_at: String,
    pub finished_at: String,
    pub minutes: f64,
    /// Input, cache writes and output. Not cache reads: see the module docs.
    pub tokens: u64,
}

/// What a pass usually takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub minutes: f64,
    pub tokens: u64,
    /// How many passes it was read from.
    pub passes: usize,
    /// Read from every project, because this one has too few passes yet.
    pub everywhere: bool,
}

impl Estimate {
    /// `≈40 min, ≈160K tokens a pass`, and where it comes from when that is
    /// not this project.
    pub fn text(&self) -> String {
        let tokens = if self.tokens >= 1_000_000 {
            format!("{:.1}M", self.tokens as f64 / 1_000_000.0)
        } else {
            format!("{}K", (self.tokens + 500) / 1000)
        };
        let scope = if self.everywhere {
            " (all projects)"
        } else {
            ""
        };
        format!(
            "≈{} min, ≈{tokens} tokens a pass{scope}",
            self.minutes.round() as u64
        )
    }
}

/// The append-only log.
pub struct OutcomeLog {
    path: PathBuf,
}

impl OutcomeLog {
    pub fn new(runtime_dir: &Path) -> Self {
        OutcomeLog {
            path: runtime_dir.join(OUTCOMES_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Add one pass. Best effort: an outcome that cannot be written costs an
    /// estimate a data point, never a verdict.
    pub fn append(&self, outcome: &QaOutcome) {
        let Ok(line) = serde_json::to_string(outcome) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Every pass recorded, oldest first. A line that does not parse is
    /// skipped.
    pub fn read(&self) -> Vec<QaOutcome> {
        fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// The estimate for each project, keyed by its name in lower case, plus the
/// one for every project under `"*"`.
pub fn estimates(outcomes: &[QaOutcome]) -> HashMap<String, Estimate> {
    // Oldest first by when each pass finished, whatever order the file holds
    // them in, so "recent" means recent. A backfill reads folders in no
    // particular order.
    let mut outcomes: Vec<&QaOutcome> = outcomes.iter().collect();
    outcomes.sort_by_key(|outcome| finished(outcome));
    let mut by_project: HashMap<String, Vec<&QaOutcome>> = HashMap::new();
    for &outcome in &outcomes {
        by_project
            .entry(outcome.project.trim().to_lowercase())
            .or_default()
            .push(outcome);
    }
    let mut out = HashMap::new();
    if let Some(everywhere) = median(&outcomes, true) {
        out.insert("*".to_string(), everywhere);
    }
    for (project, passes) in by_project {
        if let Some(estimate) = median(&passes, false) {
            out.insert(project, estimate);
        }
    }
    out
}

/// The estimate for one project: its own, or every project's while it has
/// fewer than [`MIN_PASSES`].
pub fn estimate_for(estimates: &HashMap<String, Estimate>, project: &str) -> Option<Estimate> {
    estimates
        .get(&project.trim().to_lowercase())
        .copied()
        .or_else(|| estimates.get("*").copied())
}

/// When a pass finished, for ordering. One that does not parse sorts first,
/// as the oldest, so it is the first to fall out of the window.
fn finished(outcome: &QaOutcome) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&outcome.finished_at)
        .map(|at| at.timestamp())
        .unwrap_or(i64::MIN)
}

fn median(passes: &[&QaOutcome], everywhere: bool) -> Option<Estimate> {
    let recent: Vec<&&QaOutcome> = passes.iter().rev().take(ESTIMATE_WINDOW).collect();
    if recent.len() < MIN_PASSES {
        return None;
    }
    let mut minutes: Vec<f64> = recent.iter().map(|o| o.minutes).collect();
    let mut tokens: Vec<u64> = recent.iter().map(|o| o.tokens).collect();
    minutes.sort_by(f64::total_cmp);
    tokens.sort_unstable();
    Some(Estimate {
        minutes: minutes[minutes.len() / 2],
        tokens: tokens[tokens.len() / 2],
        passes: recent.len(),
        everywhere,
    })
}
