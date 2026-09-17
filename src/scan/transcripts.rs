//! Sessions read out of the transcript store, for a machine whose process
//! table this build cannot read.
//!
//! Native Windows has no `ps` and no `lsof` here yet (see
//! [`crate::platform::LIVE_DISCOVERY`]), and until it does the sessions tab
//! would otherwise be permanently empty on a machine that is running Claude
//! Code all day. The transcripts are right there on disk and are appended to as
//! the session runs, so they answer most of the question on their own: which
//! sessions exist, where each one is running, and what it is doing — the status
//! machine only ever needed a trailing entry and an mtime.
//!
//! What a transcript cannot answer is anything about a process: no pid, no tty,
//! no start time, and so no killing, no focusing and no joining a terminal.
//! Those degrade rather than lie; see the table in the Windows section of the
//! README.
//!
//! # Why a freshness window, and not "every transcript"
//!
//! A transcript is written once and kept forever, so the store is a history,
//! not a list of what is running. Listing all of it would put months of
//! finished work in the live sessions tab — the same class of wrong as showing
//! a week-old session as live on somebody else's terminal tab, which is exactly
//! what the `starting-<pid>` rule in [`super::scanner`] was written to stop
//! (a placeholder is never handed a transcript it did not write). So a
//! transcript has to have been written to recently to count as a session at
//! all.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::transcript::SessionCwdCache;
use crate::types::RawSession;

use super::detect::is_daemon_scratch_cwd;
use super::files::{current_stat, SessionFilesCache};
use std::cmp::Reverse;

/// How recently a transcript must have been written to be listed as a session.
///
/// The status machine already draws the fine lines: ten seconds of silence
/// stops being "working", a minute of it is "idle"
/// ([`crate::util::detect_session_status`]). Everything past that reads the
/// same, so this constant is not about status — it is about whether a person
/// still thinks of that window as open. A working day is the honest bound: a
/// session touched this morning is plausibly still sitting at its prompt behind
/// another tab, and one touched last week is history. It is deliberately
/// generous, because the cost of being wrong is asymmetric — a stale row is
/// visibly idle and can be dismissed, a missing row looks like the tool is
/// broken.
pub const ACTIVITY_WINDOW: Duration = Duration::from_secs(8 * 60 * 60);

/// Every transcript under `projects_dir` written inside [`ACTIVITY_WINDOW`], as
/// sessions with no process attached, newest write first.
///
/// # Cost
///
/// One `stat` per transcript per tick, and no `read_dir` for a project
/// directory that has not changed (the listing cache), and no head read for a
/// file already seen (the cwd cache). The stat is not avoidable: an append
/// leaves the directory's mtime alone, so there is no cheaper signal that a
/// transcript is alive, and the whole point of the window is to ask. It buys
/// the one thing this layer is for, and it is only ever paid where the
/// alternative was an empty screen — on Unix this function is never called.
pub(super) fn transcript_sessions(
    projects_dir: &Path,
    files: &mut SessionFilesCache,
    cwds: &mut SessionCwdCache,
    now: SystemTime,
) -> Vec<RawSession> {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(projects_dir) {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        // No store at all is not an error: Claude Code has not run here yet.
        Err(_) => return Vec::new(),
    };
    dirs.sort();

    let mut sessions: Vec<RawSession> = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    for dir in &dirs {
        for file in files.list(dir) {
            let Some((mtime, size)) = current_stat(&file.path) else {
                continue; // deleted between the listing and now
            };
            if !within_window(mtime, now) {
                continue;
            }
            seen_files.insert(file.path.clone());
            // The transcript is the only thing that knows where it ran: the
            // directory name is a lossy encoding and cannot be decoded back.
            let cwd = cwds.get(&file.path).unwrap_or_default();
            if is_daemon_scratch_cwd(&cwd) {
                continue; // a prewarmed worker, not a session anybody started
            }
            sessions.push(RawSession {
                session_id: file
                    .name
                    .strip_suffix(".jsonl")
                    .unwrap_or(&file.name)
                    .to_string(),
                pids: Vec::new(),
                cwd,
                tty: None,
                lstart: None,
                session_file: Some(file.path.clone()),
                session_mtime: mtime,
                session_size: Some(size),
                status: None,
                starting: false,
            });
        }
    }

    files.prune(&dirs.into_iter().collect());
    cwds.prune(|path| seen_files.contains(path));

    sessions.sort_by_key(|session| Reverse(session.session_mtime));
    // One session id is one row even in the impossible case of two directories
    // holding the same file name; the newest write wins, as it does everywhere
    // else a transcript is chosen.
    sessions.dedup_by(|a, b| a.session_id == b.session_id);
    sessions
}

/// Whether `mtime` is recent enough to count. A file stamped in the future
/// counts as recent, as it does throughout the status machine, rather than
/// reading as thirty years old.
fn within_window(mtime: SystemTime, now: SystemTime) -> bool {
    now.duration_since(mtime)
        .is_ok_and(|age| age <= ACTIVITY_WINDOW)
        || mtime > now
}
