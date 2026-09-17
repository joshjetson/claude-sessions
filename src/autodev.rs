//! Auto-dev-daemon awareness.
//!
//! Ported from the Node app's `src/autodev.js`. A separate daemon drives tasks
//! autonomously and records progress as Odoo `auto_*` tags; this maps those
//! tags to a human-readable pipeline state for the board, and locates the
//! daemon's per-task run logs. Read-only: we observe, the daemon stays the
//! engine — and a machine that does not run it simply sees nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::board::AutoMarker;
use crate::types::Color;

/// One auto-dev state. `kind` is what the detail pane groups by; the board only
/// draws `marker` in `color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoState {
    pub tag: &'static str,
    pub label: &'static str,
    pub marker: &'static str,
    pub color: Color,
    pub kind: AutoKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoKind {
    Blocked,
    Paused,
    Done,
    Attention,
    Running,
}

/// Ordered by importance / pipeline advancement — the first tag a task carries
/// wins. Blocked and paused states sit on top so they override progress: a task
/// tagged both `auto_implemented` and `needs_intervention` needs intervention.
pub const STATES: &[AutoState] = &[
    state(
        "auto_abort",
        "aborted",
        "⛔ abort",
        Color::Red,
        AutoKind::Blocked,
    ),
    state(
        "needs_intervention",
        "needs intervention",
        "⚠ intervene",
        Color::Red,
        AutoKind::Blocked,
    ),
    state(
        "needs_clarification",
        "needs clarification",
        "⚠ clarify",
        Color::Red,
        AutoKind::Blocked,
    ),
    state(
        "auto_paused",
        "paused",
        "⏸ paused",
        Color::Yellow,
        AutoKind::Paused,
    ),
    state(
        "auto_qa_pass",
        "QA passed",
        "🤖✓ qa",
        Color::Green,
        AutoKind::Done,
    ),
    state(
        "auto_qa_fail",
        "QA failed → revision",
        "🤖✗ qa",
        Color::Red,
        AutoKind::Attention,
    ),
    state(
        "auto_qa_submitted",
        "in QA",
        "🤖 qa",
        Color::Cyan,
        AutoKind::Running,
    ),
    state(
        "auto_implemented",
        "implemented (MR open)",
        "🤖 impl",
        Color::Cyan,
        AutoKind::Running,
    ),
    state(
        "auto_review",
        "plan posted",
        "🤖 review",
        Color::Cyan,
        AutoKind::Running,
    ),
    state(
        "auto_decomposed",
        "decomposed",
        "🤖 split",
        Color::Cyan,
        AutoKind::Running,
    ),
    state(
        "auto_sized",
        "sized",
        "🤖 sized",
        Color::Cyan,
        AutoKind::Running,
    ),
    state(
        "auto-dev-start",
        "queued for daemon",
        "🤖 queued",
        Color::Gray,
        AutoKind::Running,
    ),
];

const fn state(
    tag: &'static str,
    label: &'static str,
    marker: &'static str,
    color: Color,
    kind: AutoKind,
) -> AutoState {
    AutoState {
        tag,
        label,
        marker,
        color,
        kind,
    }
}

/// True when a tag is one the daemon manages — "is this task in the pipeline at
/// all".
pub fn is_auto_tag(tag: &str) -> bool {
    STATES.iter().any(|state| state.tag == tag)
}

/// The current daemon state for a task's tag names, or `None`.
pub fn auto_dev_state(tags: &[String]) -> Option<&'static AutoState> {
    if tags.is_empty() {
        return None;
    }
    STATES
        .iter()
        .find(|state| tags.iter().any(|tag| tag == state.tag))
}

/// Every auto tag a task carries, in pipeline order — the detail pane's trail.
pub fn auto_dev_trail(tags: &[String]) -> Vec<&'static AutoState> {
    if tags.is_empty() {
        return Vec::new();
    }
    STATES
        .iter()
        .filter(|state| tags.iter().any(|tag| tag == state.tag))
        .collect()
}

/// The board's [`crate::board::AutoDevResolver`] slot: tags in, marker out.
///
/// A plain `fn` so [`crate::board::BoardCtx`] can hold it without a lifetime or
/// an allocation — the board draws the marker, this module decides what it is.
pub fn board_marker(tags: &[String]) -> Option<AutoMarker> {
    auto_dev_state(tags).map(|state| AutoMarker {
        marker: state.marker.to_string(),
        color: state.color,
    })
}

/// One of the daemon's run logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLog {
    pub name: String,
    pub path: PathBuf,
    pub mtime: SystemTime,
    /// The leading `<action>` of `<action>-<taskId>-<stamp>.log`.
    pub action: String,
}

/// The daemon's run logs for a task, newest first.
///
/// `runs_dir` is [`crate::paths::Paths::auto_dev_runs_dir`] — passed in rather
/// than joined onto `homedir()` here, so a test never reads the real daemon's
/// logs. A missing directory is an empty list, not an error: most machines have
/// no auto-dev-daemon at all.
pub fn list_run_logs(runs_dir: &Path, task_id: i64) -> Vec<RunLog> {
    let needle = format!("-{task_id}-");
    let Ok(entries) = fs::read_dir(runs_dir) else {
        return Vec::new();
    };
    let mut out: Vec<RunLog> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".log") || !name.contains(&needle) {
                return None;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some(RunLog {
                action: action_of(&name),
                path: entry.path(),
                name,
                mtime,
            })
        })
        .collect();
    // Newest first, name as the tiebreak so two logs written in the same
    // filesystem tick do not swap places between frames.
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    out
}

pub fn has_run_logs(runs_dir: &Path, task_id: i64) -> bool {
    !list_run_logs(runs_dir, task_id).is_empty()
}

/// `implement-6440-20260901.log` → `implement`. Node used `/-\d+-.*$/`: strip
/// from the first `-<digits>-` onwards.
fn action_of(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut i = 0;
    while let Some(offset) = name[i..].find('-') {
        let dash = i + offset;
        let mut end = dash + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > dash + 1 && end < bytes.len() && bytes[end] == b'-' {
            return name[..dash].to_string();
        }
        i = dash + 1;
    }
    name.to_string()
}

#[cfg(test)]
mod tests;
