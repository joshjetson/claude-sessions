//! The `done/` and `blocked/` directories.
//!
//! An agent signals completion by dropping a JSON file — `claude-sessions done
//! <id> --summary-file …`. A marker file rather than an HTTP call because it
//! works with no daemon running, with no network, and from inside a container:
//! the next daemon to start picks it up.
//!
//! # Polled, not watched
//!
//! The Node app used `fs.watch` with a 50ms settle. This polls both directories
//! on the refresh tick instead — a deliberate deviation. The directories are
//! empty except for the seconds around a completion, a `readdir` of an empty
//! directory is a few microseconds, and a poll has none of `fs.watch`'s
//! platform edges (missed events on network mounts, duplicate events per write,
//! a watcher that dies silently and takes every future completion with it).
//!
//! The settle survives as a mtime check: a file written less than
//! [`MARKER_SETTLE`] ago is left for the next poll, so a marker caught halfway
//! through being written is never read as truncated JSON.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::scan::ProcessSource;

use super::engine::EngineInner;

/// How long a marker must have been sitting still before it is read.
pub const MARKER_SETTLE: Duration = Duration::from_millis(50);

/// The cap the `done` helper applies to a summary. Applied again on read: the
/// file is written by whatever the agent was told to run.
const MAX_SUMMARY: usize = 8000;

/// `done/<taskId>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneMarker {
    pub task_id: i64,
    /// The directory the agent signed off in — a fact, unlike the recorded
    /// link, which is memory.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub ts: String,
}

/// `blocked/<taskId>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedMarker {
    pub task_id: i64,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub ts: String,
}

impl<S: ProcessSource> EngineInner<S> {
    /// Create both directories and delete anything left in them.
    ///
    /// Run once at startup, before the first poll: a marker from a previous run
    /// has already been acted on, and replaying it would move a task that is
    /// long since deployed and comment on it a second time.
    pub(crate) fn clean_stale_markers(&self) {
        for dir in self.marker_dirs() {
            let _ = fs::create_dir_all(&dir);
            for path in marker_files(&dir) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

impl<S: ProcessSource + Send + 'static> EngineInner<S> {
    /// Read and act on whatever has settled in the two directories.
    ///
    /// The handling runs on a worker thread: a completion makes Odoo and
    /// `glab` calls that take seconds, and the sessions list must not stop
    /// updating while they happen.
    pub(crate) fn poll_markers(self: &Arc<Self>) {
        let now = SystemTime::now();
        let [done_dir, blocked_dir] = self.marker_dirs();

        for path in settled_markers(&done_dir, now) {
            if let Some(marker) = take_marker::<DoneMarker>(&path).map(DoneMarker::capped) {
                self.spawn_worker(move |inner| inner.process_done(marker));
            }
        }
        for path in settled_markers(&blocked_dir, now) {
            if let Some(marker) = take_marker::<BlockedMarker>(&path) {
                self.spawn_worker(move |inner| inner.process_blocked(marker));
            }
        }
    }
}

/// Read a marker and remove it.
///
/// The file goes whether or not it parsed. Node left an unparseable marker on
/// disk, which under `fs.watch` meant it was simply never looked at again;
/// under polling it would be re-read on every tick for as long as the daemon
/// ran. The settle above is what keeps a half-written file from reaching here.
fn take_marker<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = fs::read_to_string(path).ok();
    let _ = fs::remove_file(path);
    serde_json::from_str(&raw?).ok()
}

/// The `.json` files in a marker directory that have stopped changing.
fn settled_markers(dir: &Path, now: SystemTime) -> Vec<PathBuf> {
    marker_files(dir)
        .into_iter()
        .filter(|path| {
            fs::metadata(path)
                .and_then(|meta| meta.modified())
                .map(|mtime| now.duration_since(mtime).unwrap_or_default() >= MARKER_SETTLE)
                .unwrap_or(false)
        })
        .collect()
}

fn marker_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

impl DoneMarker {
    /// Cap the summary, whatever wrote the file. The helper that writes it caps
    /// at the same length; this is the "whatever the agent was told to run"
    /// case, and an 8 MB paste must not become an 8 MB Odoo comment.
    pub fn capped(mut self) -> Self {
        if self.summary.len() > MAX_SUMMARY {
            let end = (0..=MAX_SUMMARY)
                .rev()
                .find(|at| self.summary.is_char_boundary(*at))
                .unwrap_or(0);
            self.summary.truncate(end);
        }
        self
    }
}
