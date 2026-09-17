//! Facts that live in a transcript's first few entries, and the cache that
//! reads each one once.
//!
//! Two things about a transcript never change after it is created: the task its
//! opening prompt links to, and the working directory it was started in. Both
//! are answered by reading the head of the file, both are asked once per file
//! per tick by code that runs every second, and both were written twice in the
//! Node app — `scanner.js`/`archive.js` for the task reference, and the cwd
//! read only in `archive.js`, which is why the sessions list had no way to say
//! where a transcript belonged without a live process to ask.
//!
//! So the read, the caching policy and the pruning live here once, and a
//! "fact" is a function from the head text to a value. See
//! [`crate::transcript::task_ref`] for the task id and [`session_cwd`] for the
//! directory.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Head bytes read when asking a transcript which directory it ran in. The
/// first entry carries it; the rest of the file is irrelevant here.
pub const CWD_HEAD_BYTES: u64 = 64 * 1024;

/// The first `limit` bytes of `path` as text.
///
/// Unreadable files are `None`, never an error: a transcript that vanished
/// mid-scan simply has no facts. Invalid UTF-8 is replaced rather than
/// rejected — a transcript is machine-written JSON, and a byte that is not text
/// is a corrupt line, not a reason to lose the file.
pub fn read_head(path: &Path, limit: u64) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(limit).read_to_end(&mut head).ok()?;
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// The working directory a transcript reports, read from the first entry in its
/// head that carries one.
///
/// The project directory name is a lossy encoding of a path — every separator
/// became a dash and the dashes already in the path were left alone — so it
/// cannot be decoded back and the file itself is the only source. That is what
/// makes a sessions list possible without a process table: the transcript knows
/// where it was started.
pub fn session_cwd(path: &Path) -> Option<String> {
    cwd_in(&read_head(path, CWD_HEAD_BYTES)?)
}

/// The first non-empty `cwd` in a head of JSONL.
///
/// A head cut mid-line leaves one unparseable line at the end; every entry is
/// its own JSON object, so skipping it costs nothing.
fn cwd_in(head: &str) -> Option<String> {
    head.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(
            |entry| match entry.get("cwd").and_then(|cwd| cwd.as_str()) {
                Some(cwd) if !cwd.is_empty() => Some(cwd.to_string()),
                _ => None,
            },
        )
}

/// One fact per transcript, with the reads remembered.
///
/// **Hits are cached forever**: the head of a transcript never changes, so one
/// read per file per process life is enough. **Misses are not cached at all** —
/// a session that has started but not yet flushed its first entry has no answer
/// on disk yet, and remembering that would make the file look task-less, or
/// directory-less, for as long as the daemon runs. Retrying a miss is the
/// caller's business (the daemon spaces those out; see the 3s task-id retry in
/// the engine's cadences).
#[derive(Debug)]
pub struct HeadCache<T> {
    hits: HashMap<PathBuf, T>,
    reads: u64,
    limit: u64,
    extract: fn(&str) -> Option<T>,
}

impl<T: Clone> HeadCache<T> {
    /// A cache for one fact: how much of the head it needs, and how to read it
    /// out of that text.
    pub fn of(limit: u64, extract: fn(&str) -> Option<T>) -> Self {
        HeadCache {
            hits: HashMap::new(),
            reads: 0,
            limit,
            extract,
        }
    }

    /// The fact for `path`, reading the file only on a miss.
    pub fn get(&mut self, path: &Path) -> Option<T> {
        if let Some(value) = self.hits.get(path) {
            return Some(value.clone());
        }
        self.reads += 1;
        let value = (self.extract)(&read_head(path, self.limit)?)?;
        self.hits.insert(path.to_path_buf(), value.clone());
        Some(value)
    }

    /// Files read from disk so far — the counter the O(1) claims are tested
    /// against, and worth a diagnostics line if it ever climbs per tick.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// Drop remembered hits for files that no longer exist.
    pub fn prune(&mut self, exists: impl Fn(&Path) -> bool) {
        self.hits.retain(|path, _| exists(path));
    }
}

/// The working directory of each transcript the scanner has seen.
pub type SessionCwdCache = HeadCache<String>;

impl SessionCwdCache {
    pub fn new() -> Self {
        HeadCache::of(CWD_HEAD_BYTES, cwd_in)
    }
}

impl Default for SessionCwdCache {
    fn default() -> Self {
        SessionCwdCache::new()
    }
}
