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

use std::path::Path;

use super::head::{read_head, HeadCache};

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
    task_ref_in(&read_head(path, TASK_REF_HEAD_BYTES)?)
}

/// [`first_task_ref`] with the reads remembered. The hit-and-miss policy, the
/// read counter and the pruning are [`HeadCache`]'s, and are what this type is
/// for — a transcript's opening prompt never changes, so the daemon reads each
/// file once per process life.
pub type TaskRefCache = HeadCache<i64>;

impl TaskRefCache {
    pub fn new() -> Self {
        HeadCache::of(TASK_REF_HEAD_BYTES, task_ref_in)
    }
}

impl Default for TaskRefCache {
    fn default() -> Self {
        TaskRefCache::new()
    }
}
