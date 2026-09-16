//! Working out which transcript each `claude` process is writing.
//!
//! This is the part of the scanner that has been wrong in production twice, so
//! the reasoning is written down rather than implied.
//!
//! It began as rank pairing: Nth-newest file to Nth-newest process. The two
//! orderings are independent — a session resumed days ago and typed into a
//! moment ago is the newest file AND the oldest process — so in any repo with
//! more than one session the ttys shuffled and "go to this session's terminal"
//! opened somebody else's tab. Measured against processes whose session id was
//! on their own command line, 8 of 10 live sessions pointed somewhere else.
//!
//! Identity first, then creation time, and only then the old ranking.
//!
//! The function is pure — processes in, files in, pairs out, with the one piece
//! of file reading it needs (the task a transcript names) passed in as a
//! closure. That is what lets the Node suite's fixtures drive it with no
//! processes and no `ps` anywhere near the test.

use std::path::Path;
use std::time::SystemTime;

use crate::types::SessionFile;

use super::process::ClaudeProcess;

/// `ps` reports a start time truncated to the second, so a reported start is
/// never LATER than the real one: a transcript born before it predates the
/// process for real, and the slack only has to cover clock jitter.
///
/// It was 2000ms, which is longer than the gap between two launches made
/// back-to-back — it let each transcript be attributed to the process that
/// started just after it instead of the one that wrote it.
pub const BIRTH_SLACK_MS: i64 = 500;

/// How long after starting a process may still be opening its transcript. A
/// trust prompt or a slow start delays it, but not indefinitely.
pub const BIRTH_WINDOW_MS: i64 = 15 * 60 * 1000;

/// One process matched to one file, by index into the slices handed in.
///
/// Indices rather than references: several processes legitimately share one
/// transcript, and the caller wants both sides back without fighting the
/// borrow checker over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pairing {
    pub proc_index: usize,
    pub file_index: usize,
}

/// `later - earlier` in milliseconds, signed.
fn millis_between(earlier: SystemTime, later: SystemTime) -> i64 {
    match later.duration_since(earlier) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Pair processes to the transcripts they are writing.
///
/// `procs` arrive newest-started first and `files` newest-mtime first, which is
/// what makes "the first candidate" mean "the most recent one" below.
///
/// `task_ref` answers "which task does this transcript's opening prompt name?"
/// — normally a [`crate::transcript::TaskRefCache`], so a file is read once and
/// hits are remembered for good.
pub fn pair_processes_to_sessions(
    procs: &[ClaudeProcess],
    files: &[SessionFile],
    task_ref: &mut dyn FnMut(&Path) -> Option<i64>,
) -> Vec<Pairing> {
    let mut pairs: Vec<Pairing> = Vec::new();
    let mut claimed = vec![false; files.len()];
    let mut unmatched: Vec<usize> = Vec::with_capacity(procs.len());

    // 1. Exact: the id the process names on its own command line.
    for (pi, proc) in procs.iter().enumerate() {
        let found = proc.session_id.as_deref().and_then(|id| {
            files
                .iter()
                .rposition(|f| f.name.strip_suffix(".jsonl") == Some(id))
        });
        match found {
            // Several processes on one resumed session (the racing case) all
            // belong to it; the newest keeps the session's tty because procs
            // arrive newest-first.
            Some(fi) => {
                pairs.push(Pairing {
                    proc_index: pi,
                    file_index: fi,
                });
                claimed[fi] = true;
            }
            None => unmatched.push(pi),
        }
    }

    // 1b. Exact: the task the process was launched for, against the task each
    //     transcript names in its own opening prompt.
    //
    //     Birth time cannot separate launches made seconds apart. Eight tasks
    //     were started from one folder within 20 seconds on 2026-09-15, and
    //     every transcript was born about 1.7s after its OWN process but only
    //     0.3s before the NEXT one — so the closest-pair rule below handed each
    //     process its successor's transcript and seven of eight sessions showed
    //     the wrong task, the wrong tty and the wrong terminal tab. This needs
    //     no timing at all: the launch states its task, and so does the
    //     transcript.
    unmatched.retain(|&pi| {
        let Some(task) = procs[pi].launch_task_id else {
            return true;
        };
        // Newest first, so a task on its second round takes the current
        // transcript rather than the one it wrote last time.
        let found =
            (0..files.len()).find(|&fi| !claimed[fi] && task_ref(&files[fi].path) == Some(task));
        match found {
            Some(fi) => {
                pairs.push(Pairing {
                    proc_index: pi,
                    file_index: fi,
                });
                claimed[fi] = true;
                false
            }
            None => true,
        }
    });

    // 2. Fresh sessions, matched by when their transcript was created: a
    //    process writes its own within a second or two of starting. Closest
    //    pair first, GLOBALLY — taking each process's earliest candidate in
    //    turn lets one that never wrote a file (a headless helper, or one still
    //    at the trust prompt) swallow the next process's transcript and push
    //    the error along the whole chain.
    let mut candidates: Vec<(i64, usize, usize)> = Vec::new();
    for &pi in &unmatched {
        let proc = &procs[pi];
        let Some(start) = proc.start else {
            continue;
        };
        for (fi, file) in files.iter().enumerate() {
            if claimed[fi] {
                continue;
            }
            // Unknown creation time, which is not the same as "born at the
            // epoch" — such a file can only be reached by rank pairing.
            let Some(birth) = file.birthtime else {
                continue;
            };
            let delta = millis_between(start, birth);
            if !(-BIRTH_SLACK_MS..=BIRTH_WINDOW_MS).contains(&delta) {
                continue;
            }
            // A launch states its task. A transcript that names a DIFFERENT one
            // cannot be this process's, however close the two timestamps are.
            //
            // Asked AFTER the window check, which Node did the other way round:
            // the answer costs a 128 KiB head read for every file that has no
            // reference to cache, and a transcript from outside the window is
            // not a candidate whatever task it names.
            if let Some(task) = proc.launch_task_id {
                if matches!(task_ref(&file.path), Some(ref_id) if ref_id != task) {
                    continue;
                }
            }
            candidates.push((delta.abs(), pi, fi));
        }
    }
    // Stable, so equal deltas resolve in the order the caller handed them over
    // — the same input always produces the same pairing.
    candidates.sort_by_key(|(delta, _, _)| *delta);
    let mut matched = vec![false; procs.len()];
    for (_, pi, fi) in candidates {
        if matched[pi] || claimed[fi] {
            continue;
        }
        pairs.push(Pairing {
            proc_index: pi,
            file_index: fi,
        });
        claimed[fi] = true;
        matched[pi] = true;
    }
    unmatched.retain(|&pi| !matched[pi]);

    // 3. Only if the filesystem cannot say when a file was created: the
    //    original newest-file-to-newest-process ranking. Where creation times
    //    ARE known, an unmatched process has simply not written its transcript
    //    yet (it is still at the trust prompt) and is reported as starting.
    //    Guessing here is what showed a week-old session as live on somebody
    //    else's tab.
    if !files.iter().any(|f| f.birthtime.is_some()) {
        let spare: Vec<usize> = (0..files.len()).filter(|&fi| !claimed[fi]).collect();
        unmatched.sort_by(|&a, &b| procs[b].start.cmp(&procs[a].start));
        for (&pi, &fi) in unmatched.iter().zip(spare.iter()) {
            pairs.push(Pairing {
                proc_index: pi,
                file_index: fi,
            });
            claimed[fi] = true;
        }
    }

    pairs
}
