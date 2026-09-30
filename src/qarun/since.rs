//! Which recorded QA results belong to this run, and which to an earlier one.
//!
//! A run over a stage has the same id every time, and QAden keeps one
//! `run.json` per task. So when a run starts, every task it covers may already
//! have a verdict on disk from an earlier pass, and it keeps that verdict until
//! the new pass starts its round, minutes later. A coordinator started first
//! read those files, saw verdicts, and reported the earlier pass as this run's
//! result.
//!
//! The file's write time settles it. QAden writes `run.json` when a pass starts
//! or resumes a round and whenever it records a result, so a file written
//! before the run started holds nothing from this run. That rule lives here, in
//! code, and `claude-sessions qa-status` prints it, so the coordinator reads the
//! answer rather than working it out from timestamps.

use std::fs;
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::paths::Paths;
use crate::qaden::{qa_run_state, QaVerdict};

/// Where a task's QA record stands, measured against the start of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// No `run.json` at all.
    Nothing,
    /// A `run.json` this tool cannot parse. Never read as "not started": the
    /// pass may well have run.
    Unreadable,
    /// Written before the run started: an earlier pass.
    Earlier,
    /// Written since the run started.
    ThisRun,
}

/// A task's QA record, and whether it belongs to the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub task_id: i64,
    pub recorded: Recorded,
    pub round: u32,
    pub verdict: Option<QaVerdict>,
    pub written: Option<SystemTime>,
}

/// Read a task's record and place it against `since`, the run's start.
///
/// A file whose write time cannot be read counts as earlier. Calling an old
/// verdict new is the failure this exists to stop, so the doubt goes that way.
pub fn task_record(paths: &Paths, task_id: i64, since: SystemTime) -> TaskRecord {
    let state = qa_run_state(paths, task_id, |_| None);
    let written = fs::metadata(state.dir.join("run.json"))
        .and_then(|meta| meta.modified())
        .ok();
    let recorded = match (state.exists, written) {
        (false, None) => Recorded::Nothing,
        (false, Some(_)) => Recorded::Unreadable,
        (true, Some(at)) if at >= since => Recorded::ThisRun,
        (true, _) => Recorded::Earlier,
    };
    TaskRecord {
        task_id,
        recorded,
        round: state.round,
        verdict: state.verdict,
        written,
    }
}

/// One line for the coordinator, saying plainly which run a result is from.
pub fn record_line(record: &TaskRecord) -> String {
    let id = record.task_id;
    let verdict = record.verdict.map_or("no verdict yet", QaVerdict::as_str);
    let written = record.written.map(iso).unwrap_or_default();
    match record.recorded {
        Recorded::Nothing => format!("#{id} not started: no QA record yet"),
        Recorded::Unreadable => format!(
            "#{id} unknown: its run.json (written {written}) could not be read. \
             Open the file yourself before you report this task."
        ),
        Recorded::ThisRun => format!(
            "#{id} this run: round {}, {verdict} (run.json written {written})",
            record.round
        ),
        Recorded::Earlier => format!(
            "#{id} not started in this run: the record is from an earlier pass \
             (round {}, {verdict}, written {written}, before this run started). \
             It is NOT this run's result.",
            record.round
        ),
    }
}

fn iso(at: SystemTime) -> String {
    DateTime::<Utc>::from(at).to_rfc3339_opts(SecondsFormat::Secs, true)
}
