//! The shadow record: what a coordinating session WOULD have answered.
//!
//! It exists to produce one number — how often the coordinator's answer matches
//! the reviewer's — because that number is the only honest basis for letting it
//! answer anything unsupervised.
//!
//! No such number existed when this was designed. The evidence available was one
//! recorded checkpoint redirect in seventy-five runs, and that field records
//! disagreement rather than interaction, so it is a floor and not a rate. A
//! triage layer justified on that would be justified on nothing.
//!
//! # The ordering rule
//!
//! The coordinator's answer is written BEFORE the reviewer's is known, and this
//! module refuses to change one afterwards. An answer that can be edited once
//! the real one arrives measures nothing. That is the same reason QAden records
//! its coverage challenge before a verdict exists and will not let the gap list
//! be shortened after.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One recorded prediction, and the real answer once it is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowRecord {
    pub run_id: String,
    pub task_id: i64,
    pub question: String,
    pub would_answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    pub recorded_at: String,
    /// Filled in later, by a person. The only field writable after the fact,
    /// and the one the coordinator never supplies.
    #[serde(default)]
    pub actual_answer: Option<String>,
    #[serde(default)]
    pub agreed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowError {
    /// A prediction for this task already exists. A second opinion formed after
    /// seeing something new is not a prediction, and the one that counts is the
    /// one formed first.
    AlreadyRecorded(i64),
    /// "I do not know" is a valid answer and a real data point. An empty one is
    /// neither.
    EmptyAnswer,
    NotRecorded(i64),
    Io(String),
}

impl ShadowError {
    pub fn detail(&self) -> String {
        match self {
            ShadowError::AlreadyRecorded(id) => format!(
                "a shadow answer for task {id} already exists — it is not editable by design"
            ),
            ShadowError::EmptyAnswer => {
                "an answer is required — \"I do not know: <why>\" is a real data point, an empty answer is not"
                    .to_string()
            }
            ShadowError::NotRecorded(id) => {
                format!("no shadow answer was recorded for task {id}")
            }
            ShadowError::Io(message) => message.clone(),
        }
    }
}

/// One flat directory per run, under the runtime directory.
///
/// A run id carries a project name and a stage name, both free text out of
/// Odoo, so it is never a safe path segment. Replacing the unsafe characters is
/// not enough on its own: `.` and `..` survive that pass unchanged, and joining
/// `..` onto a directory is its parent. A segment that is only dots, or empty,
/// is therefore replaced outright.
fn safe_segment(run_id: &str) -> String {
    let cleaned: String = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return "_run".to_string();
    }
    cleaned
}

pub struct ShadowStore {
    root: PathBuf,
}

impl ShadowStore {
    pub fn new(runtime_dir: &Path) -> Self {
        ShadowStore {
            root: runtime_dir.join("qa-shadow"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.join(safe_segment(run_id))
    }

    fn record_path(&self, run_id: &str, task_id: i64) -> PathBuf {
        self.run_dir(run_id).join(format!("task-{task_id}.json"))
    }

    /// Record what the coordinator would have answered. Refuses to overwrite.
    pub fn record(
        &self,
        run_id: &str,
        task_id: i64,
        question: &str,
        would_answer: &str,
        confidence: Option<&str>,
        recorded_at: String,
    ) -> Result<PathBuf, ShadowError> {
        if would_answer.trim().is_empty() {
            return Err(ShadowError::EmptyAnswer);
        }
        let path = self.record_path(run_id, task_id);
        if path.exists() {
            return Err(ShadowError::AlreadyRecorded(task_id));
        }
        fs::create_dir_all(self.run_dir(run_id)).map_err(|e| ShadowError::Io(e.to_string()))?;

        let record = ShadowRecord {
            run_id: run_id.to_string(),
            task_id,
            question: question.to_string(),
            would_answer: would_answer.to_string(),
            confidence: confidence.map(str::to_string),
            recorded_at,
            actual_answer: None,
            agreed: None,
        };
        let json =
            serde_json::to_string_pretty(&record).map_err(|e| ShadowError::Io(e.to_string()))?;
        fs::write(&path, json).map_err(|e| ShadowError::Io(e.to_string()))?;
        Ok(path)
    }

    /// Attach the real answer. Never rewrites the prediction.
    pub fn score(
        &self,
        run_id: &str,
        task_id: i64,
        actual_answer: Option<&str>,
        agreed: Option<bool>,
    ) -> Result<(), ShadowError> {
        let path = self.record_path(run_id, task_id);
        let raw = fs::read_to_string(&path).map_err(|_| ShadowError::NotRecorded(task_id))?;
        let mut record: ShadowRecord =
            serde_json::from_str(&raw).map_err(|e| ShadowError::Io(e.to_string()))?;

        record.actual_answer = actual_answer.map(str::to_string);
        record.agreed = agreed;
        let json =
            serde_json::to_string_pretty(&record).map_err(|e| ShadowError::Io(e.to_string()))?;
        fs::write(&path, json).map_err(|e| ShadowError::Io(e.to_string()))
    }

    pub fn records(&self, run_id: &str) -> Vec<ShadowRecord> {
        let Ok(entries) = fs::read_dir(self.run_dir(run_id)) else {
            return Vec::new();
        };
        let mut out: Vec<ShadowRecord> = entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("task-") && name.ends_with(".json")
            })
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .filter_map(|raw| serde_json::from_str::<ShadowRecord>(&raw).ok())
            .collect();
        out.sort_by_key(|record| record.task_id);
        out
    }

    pub fn agreement(&self, run_id: &str) -> Agreement {
        Agreement::of(&self.records(run_id))
    }
}

/// How well the coordinator has matched the reviewer, so far.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Agreement {
    pub recorded: usize,
    pub scored: usize,
    pub agreed: usize,
    pub unscored: usize,
}

impl Agreement {
    fn of(records: &[ShadowRecord]) -> Self {
        let scored = records.iter().filter(|r| r.agreed.is_some()).count();
        Agreement {
            recorded: records.len(),
            scored,
            agreed: records.iter().filter(|r| r.agreed == Some(true)).count(),
            unscored: records.len() - scored,
        }
    }

    /// `None` rather than zero when nothing has been scored.
    ///
    /// A rate of 0% and "no data" are opposite conclusions, and a decision that
    /// cannot tell them apart will be made on the wrong one.
    pub fn rate(&self) -> Option<f64> {
        (self.scored > 0).then(|| self.agreed as f64 / self.scored as f64)
    }

    /// What to show where someone decides whether to trust it.
    pub fn summary(&self) -> String {
        match self.rate() {
            None => format!("no answers scored yet ({} recorded)", self.recorded),
            Some(rate) => format!(
                "{:.0}% of {} scored ({} recorded, {} unscored)",
                rate * 100.0,
                self.scored,
                self.recorded,
                self.unscored
            ),
        }
    }
}
