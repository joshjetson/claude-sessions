//! A running `claude` process, before and after its transcript is read.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::{CumulativeUsage, LastEntry, Prompt, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Working,
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
    pub pids: Vec<u32>,
    /// Kept as a string, not a `PathBuf`: it arrives as `lsof` output, is used
    /// as a grouping key, and is encoded into a project directory name.
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    /// Raw `ps -o lstart` text; formatted lazily by `util::format_start_time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lstart: Option<String>,
    /// `None` for a `starting-<pid>` placeholder that has no transcript yet.
    /// (Node used an empty string; the pairing rules hang off this being unset.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
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
    pub pids: Vec<u32>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lstart: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    pub session_mtime: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_size: Option<u64>,
    pub status: SessionStatus,
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
}

/// One `*.jsonl` in a project's transcript directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFile {
    pub name: String,
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub size: u64,
}
