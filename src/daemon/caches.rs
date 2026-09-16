//! The caches one tick reads through, and the cadences they expire on.
//!
//! The cadences are the brief's table: compacting 2s, a task-reference miss
//! retried after 3s, project discovery 30s. The transcript cursors have no
//! cadence at all — they are the steady-state parse path (mandate #1) and live
//! for exactly as long as their session does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::scan::{discover_projects, is_compacting};
use crate::transcript::{Collect, TaskRefCache, TranscriptCursor};
use crate::types::ParsedSession;

/// How long `is_compacting`'s answer is reused. It reads a directory, and a
/// compaction lasts far longer than a tick.
const COMPACTING_TTL: Duration = Duration::from_secs(2);
/// How long a session that showed no task reference is taken at its word.
///
/// A hit is permanent and is remembered by the shared [`TaskRefCache`] — the
/// reference comes from the spawn prompt at the head of the file and cannot
/// change. A miss is not: a session seen in the split second before its first
/// prompt is flushed would otherwise read as task-less for this daemon's whole
/// life, and everything keyed off the id — linking, auto-archive on vanish,
/// "go to this task's session" — would quietly skip it.
const TASK_REF_RETRY: Duration = Duration::from_secs(3);
/// How long a group's repository listing is reused.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);

/// A value with an expiry.
#[derive(Debug, Clone)]
struct Fresh<T> {
    until: SystemTime,
    value: T,
}

impl<T> Fresh<T> {
    fn get(&self, now: SystemTime) -> Option<&T> {
        (now < self.until).then_some(&self.value)
    }
}

/// Everything a tick remembers about the files it read.
#[derive(Debug, Default)]
pub(crate) struct Caches {
    /// One open cursor per live transcript. THE reason this port exists: Node
    /// re-read the last 512 KB of every live transcript and re-parsed it from
    /// scratch once a second, to learn about the handful of lines that had
    /// actually arrived.
    cursors: HashMap<PathBuf, TranscriptCursor>,
    compacting: HashMap<PathBuf, Fresh<bool>>,
    /// Only misses are kept here; hits live in the scanner's shared cache.
    task_ref_retry: HashMap<PathBuf, SystemTime>,
    discovery: HashMap<PathBuf, Fresh<Vec<String>>>,
    opened: u64,
}

impl Caches {
    /// Fold whatever this transcript has gained since the last tick.
    ///
    /// The first sight of a file opens a cursor (which reads the tail once);
    /// every tick after that costs the size of the append. A file that cannot
    /// be opened — it vanished mid-tick — is not remembered, so the next tick
    /// tries again rather than writing it off.
    pub(crate) fn session(&mut self, path: &Path) -> Option<ParsedSession> {
        if let Some(cursor) = self.cursors.get_mut(path) {
            let _ = cursor.poll();
            return Some(cursor.session());
        }
        // The sessions list never renders the conversation itself, so the
        // daemon does not hold every message of every open session in memory.
        let cursor = TranscriptCursor::open(path, Collect::Session).ok()?;
        self.opened += 1;
        let parsed = cursor.session();
        self.cursors.insert(path.to_path_buf(), cursor);
        Some(parsed)
    }

    /// Whether this session is busy summarising itself.
    pub(crate) fn compacting(&mut self, path: &Path, now: SystemTime) -> bool {
        if let Some(cached) = self.compacting.get(path).and_then(|f| f.get(now)) {
            return *cached;
        }
        let value = is_compacting(path, now);
        self.compacting.insert(
            path.to_path_buf(),
            Fresh {
                until: now + COMPACTING_TTL,
                value,
            },
        );
        value
    }

    /// The task a transcript's opening prompt names, asked of the scanner's
    /// cache so a file is read once per process life whichever subsystem asked
    /// first — with the retry TTL on top for the not-yet-written case.
    pub(crate) fn task_id(
        &mut self,
        path: &Path,
        refs: &mut TaskRefCache,
        now: SystemTime,
    ) -> Option<i64> {
        if let Some(retry_at) = self.task_ref_retry.get(path) {
            if now < *retry_at {
                return None;
            }
        }
        match refs.get(path) {
            Some(id) => {
                self.task_ref_retry.remove(path);
                Some(id)
            }
            None => {
                self.task_ref_retry
                    .insert(path.to_path_buf(), now + TASK_REF_RETRY);
                None
            }
        }
    }

    /// The repository folders inside a configured group.
    pub(crate) fn discover(&mut self, group: &Path, force: bool, now: SystemTime) -> Vec<String> {
        if !force {
            if let Some(dirs) = self.discovery.get(group).and_then(|f| f.get(now)) {
                return dirs.clone();
            }
        }
        let dirs = discover_projects(group);
        self.discovery.insert(
            group.to_path_buf(),
            Fresh {
                until: now + DISCOVERY_TTL,
                value: dirs.clone(),
            },
        );
        dirs
    }

    /// Forget everything about files that are no longer live.
    ///
    /// Dropping the cursor is what closes the file: a daemon that ran for a
    /// week would otherwise hold one descriptor per session it had ever seen.
    pub(crate) fn prune(&mut self, live: &HashSet<PathBuf>) {
        self.cursors.retain(|path, _| live.contains(path));
        self.compacting.retain(|path, _| live.contains(path));
        self.task_ref_retry.retain(|path, _| live.contains(path));
    }

    /// Cursors currently open — one per live session, which is the invariant
    /// the tests assert rather than take on trust.
    pub(crate) fn open_cursors(&self) -> usize {
        self.cursors.len()
    }

    /// Transcripts opened since the engine started. A counter that climbs once
    /// per tick would mean the cold-start path is running every time.
    #[cfg(test)]
    pub(crate) fn opened(&self) -> u64 {
        self.opened
    }

    /// Bytes every live cursor has read, cold starts included — what the
    /// O(appended-bytes) promise is measured against.
    #[cfg(test)]
    pub(crate) fn bytes_read(&self) -> u64 {
        self.cursors.values().map(|c| c.total_bytes_read()).sum()
    }
}
