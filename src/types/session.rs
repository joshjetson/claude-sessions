//! A running `claude` process, before and after its transcript is read.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::wire;
use super::{CumulativeUsage, LastEntry, Prompt, Usage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Working,
    #[default]
    Idle,
    /// A tool call that has been in flight long enough to be waiting on
    /// something outside the session — most often a permission prompt.
    Awaiting,
    AwaitingInput,
    Compacting,
    Starting,
}

/// A claude process matched to its transcript file, before parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSession {
    pub session_id: String,
    #[serde(deserialize_with = "wire::pids")]
    pub pids: Vec<u32>,
    /// Kept as a string, not a `PathBuf`: it arrives as `lsof` output, is used
    /// as a grouping key, and is encoded into a project directory name.
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    /// Raw `ps -o lstart` text; formatted lazily by `util::format_start_time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lstart: Option<String>,
    /// `CLAUDE_SESSIONS_RUN_ID`, present only on a QA run's coordinator.
    ///
    /// The coordinator holds no task id, so this is what tells it apart from
    /// the run's own QA sessions, which start in the same folder moments later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `None` for a `starting-<pid>` placeholder that has no transcript yet.
    /// (Node used an empty string; the pairing rules hang off this being unset.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    /// Epoch milliseconds on the wire, which is what Node sends and sorts by.
    #[serde(with = "wire::epoch_ms")]
    pub session_mtime: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SessionStatus>,
    #[serde(default)]
    pub starting: bool,
}

/// A raw session enriched with everything parsed out of its transcript. The
/// parsed session id wins over the process-derived one, so the two sources are
/// merged rather than intersected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    #[serde(deserialize_with = "wire::pids")]
    pub pids: Vec<u32>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lstart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    /// Epoch milliseconds on the wire, which is what Node sends and sorts by.
    #[serde(with = "wire::epoch_ms")]
    pub session_mtime: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_size: Option<u64>,
    /// Computed by whoever owns the transcript — the daemon before it wires the
    /// session, the embedded scan otherwise — and rendered verbatim by the
    /// client, which has no `lastEntry` to recompute it from. Defaulted rather
    /// than required, exactly as Node's client did it (`s.status || 'idle'`).
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub activity_detail: String,
    #[serde(default)]
    pub starting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_usage: Option<Usage>,
    /// Stripped before this crosses the daemon boundary — see the wire rules in
    /// the brief; it exists so the daemon can re-derive status without re-parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_entry: Option<LastEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cumulative_usage: Option<CumulativeUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<Prompt>,
    /// Odoo task this session is working, read from its transcript head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<i64>,
    /// The QA run this session coordinates, from `CLAUDE_SESSIONS_RUN_ID`.
    ///
    /// Set on a coordinator and on nothing else, which is what makes it a
    /// reliable way to find one. Carried from the process environment rather
    /// than the transcript, so it is known before a single line is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// One `*.jsonl` in a project's transcript directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFile {
    pub name: String,
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub size: u64,
    /// Creation time, where the filesystem reports one. `None` is "unknown",
    /// which the pairing rules treat as a different thing from "born at the
    /// epoch" — Node conflated the two behind `birthtime > 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birthtime: Option<SystemTime>,
}
