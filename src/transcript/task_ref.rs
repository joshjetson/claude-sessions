//! The task a transcript's opening prompt names.
//!
//! A dashboard-launched session is spawned with a prompt that links back to its
//! Odoo task (`…#id=6137&model=project.task&view_type=form`), so the first few
//! kilobytes of a transcript state which task it belongs to. That is the only
//! link between a transcript and a task that needs no timing and no guesswork,
//! and two parts of the app need it: pairing processes to transcripts
//! ([`crate::scan`]) and deciding whether an archive candidate belongs to some
//! other task (the archive module). Node wrote it twice — `scanner.js`
//! `firstTaskRefOf` and `archive.js` `firstTaskRef`, with different cache rules
//! — so it lives here once, as transcript knowledge.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a transcript's head is searched. The spawn prompt is the first
/// entry; 128 KiB covers it with room for a pasted context block in front.
pub const TASK_REF_HEAD_BYTES: u64 = 128 * 1024;

/// The Odoo task id referenced in `text`, or `None`.
///
/// Node's regex, verbatim: `/id=(\d+)&model=project\.task/`.
pub fn task_ref_in(text: &str) -> Option<i64> {
    for (start, _) in text.match_indices("id=") {
        let rest = &text[start + 3..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            continue;
        }
        if !rest[digits..].starts_with("&model=project.task") {
            continue;
        }
        if let Ok(id) = rest[..digits].parse::<i64>() {
            return Some(id);
        }
    }
    None
}

/// Read the head of `path` and return the task id its opening prompt names.
///
/// Unreadable files are `None`, never an error: a transcript that vanished
/// mid-scan simply has no task.
pub fn first_task_ref(path: &Path) -> Option<i64> {
    let file = File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(TASK_REF_HEAD_BYTES).read_to_end(&mut head).ok()?;
    task_ref_in(&String::from_utf8_lossy(&head))
}

/// [`first_task_ref`] with the reads remembered.
///
/// **Hits are cached forever**: a transcript's opening prompt never changes, so
/// one read per file per process life is enough. **Misses are not cached at
/// all** — a session that has started but not yet flushed its first prompt has
/// no reference on disk yet, and remembering that would make the file look
/// task-less for as long as the daemon runs. Retrying a miss is the caller's
/// business (the daemon spaces those out; see the 3s task-id retry in the
/// engine's cadences).
#[derive(Debug, Default)]
pub struct TaskRefCache {
    hits: HashMap<PathBuf, i64>,
    reads: u64,
}

impl TaskRefCache {
    pub fn new() -> Self {
        TaskRefCache::default()
    }

    /// The task id for `path`, reading the file only on a miss.
    pub fn get(&mut self, path: &Path) -> Option<i64> {
        if let Some(id) = self.hits.get(path) {
            return Some(*id);
        }
        self.reads += 1;
        let id = first_task_ref(path)?;
        self.hits.insert(path.to_path_buf(), id);
        Some(id)
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
