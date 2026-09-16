//! Pairing processes to transcripts.
//!
//! The dashboard used to pair these by rank: Nth-newest file to Nth-newest
//! process. The two orderings are independent, so in any repo with more than
//! one session the ttys shuffled and "go to this session's terminal" opened the
//! wrong tab. Measured against processes whose session id was on their command
//! line, 8 of 10 live sessions pointed somewhere else.

use crate::scan::pair_processes_to_sessions;
use crate::types::SessionFile;

use super::{a_file, a_proc, pair, ClaudeProcess};

#[test]
fn the_reported_bug_the_oldest_process_owns_the_most_recently_written_file() {
    // A session resumed days ago and typed into a moment ago is the newest file
    // AND the oldest process. Rank pairing gets both of these backwards.
    let procs = [
        ClaudeProcess {
            tty: Some("ttys002".to_string()),
            session_id: Some("b".to_string()),
            ..a_proc(NEW, 9_000)
        },
        ClaudeProcess {
            session_id: Some("a".to_string()),
            ..a_proc(OLD, 1_000)
        },
    ];
    let files = [
        a_file("a", 9_999, Some(500)), // busiest, so listed first
        a_file("b", 2_000, Some(9_100)),
    ];
    assert_eq!(
        pair(&procs, &files),
        vec![(NEW, "b".to_string()), (OLD, "a".to_string())]
    );
}

const NEW: u32 = 2;
const OLD: u32 = 1;
const P1: u32 = 100;

#[test]
fn session_id_identifies_a_fresh_session_as_exactly_as_resume() {
    let procs = [ClaudeProcess {
        session_id: Some("declared".to_string()),
        ..a_proc(P1, 1_000_000)
    }];
    let files = [
        a_file("other", 2_000_000, Some(1_000_500)),
        a_file("declared", 2_000_000, Some(5)),
    ];
    assert_eq!(pair(&procs, &files), vec![(P1, "declared".to_string())]);
}

#[test]
fn a_process_with_no_declared_id_takes_the_transcript_born_just_after_it() {
    let procs = [a_proc(P1, 10_000)];
    let files = [
        a_file("old", 2_000_000, Some(1_000)),
        a_file("mine", 2_000_000, Some(12_000)),
    ];
    assert_eq!(pair(&procs, &files), vec![(P1, "mine".to_string())]);
}

#[test]
fn a_transcript_created_before_the_process_started_is_never_claimed_by_it() {
    let procs = [a_proc(P1, 10_000)];
    let files = [a_file("older", 2_000_000, Some(1_000))];
    assert!(pair(&procs, &files).is_empty());
}

#[test]
fn a_process_that_has_written_nothing_yet_is_left_unpaired_not_given_an_old_file() {
    // It is still at the trust prompt. Binding it to whatever file was lying
    // around is what showed a week-old session as live on a stranger's tab.
    let procs = [a_proc(P1, 10_000)];
    let files = [a_file("ancient", 2_000_000, Some(1_000))];
    assert!(pair(&procs, &files).is_empty());
}

#[test]
fn the_closest_pair_wins_globally_so_one_silent_process_cannot_shift_the_rest() {
    // A headless helper starts alongside a real session; taking each process's
    // earliest candidate in turn let it swallow the next one's transcript and
    // push every later pairing one file along.
    const LATER: u32 = 7;
    const SILENT: u32 = 8;
    let procs = [a_proc(LATER, 100_000), a_proc(SILENT, 1_000)];
    let files = [a_file("lateFile", 2_000_000, Some(102_000))];
    assert_eq!(pair(&procs, &files), vec![(LATER, "lateFile".to_string())]);
}

#[test]
fn several_processes_on_one_resumed_session_all_belong_to_it() {
    // The racing case: nine agents once ran against a single conversation.
    const A: u32 = 11;
    const B: u32 = 12;
    let procs = [
        ClaudeProcess {
            session_id: Some("shared".to_string()),
            ..a_proc(A, 3_000)
        },
        ClaudeProcess {
            session_id: Some("shared".to_string()),
            ..a_proc(B, 2_000)
        },
    ];
    let files = [a_file("shared", 2_000_000, Some(1_000_000))];
    let paired = pair_processes_to_sessions(&procs, &files, &mut |_| None);
    assert_eq!(paired.len(), 2);
    assert_eq!(
        procs[paired[0].proc_index].pid, A,
        "the newest process should be listed first"
    );
    assert!(paired
        .iter()
        .all(|p| files[p.file_index].name == "shared.jsonl"));
}

#[test]
fn with_no_creation_times_available_it_falls_back_to_rank_pairing() {
    // Not every filesystem reports one; the old behaviour is the safety net.
    let procs = [
        a_proc(NEW, 1_700_000_009_000),
        a_proc(OLD, 1_700_000_001_000),
    ];
    let files = [
        a_file("newest", 2_000_000, None),
        a_file("older", 1_000_000, None),
    ];
    assert_eq!(
        pair(&procs, &files),
        vec![(NEW, "newest".to_string()), (OLD, "older".to_string())]
    );
}

#[test]
fn rank_pairing_stays_switched_off_as_soon_as_one_file_reports_a_birth_time() {
    // Mixed knowledge is not ignorance: guessing for the rest is what the
    // fallback is there to avoid.
    let procs = [
        a_proc(NEW, 1_700_000_009_000),
        a_proc(OLD, 1_700_000_001_000),
    ];
    let files = [
        a_file("known", 2_000_000, Some(1_700_000_009_500)),
        a_file("unknown", 1_000_000, None),
    ];
    assert_eq!(pair(&procs, &files), vec![(NEW, "known".to_string())]);
}

#[test]
fn no_processes_and_no_files_are_both_fine() {
    let files = [a_file("a", 2_000_000, Some(1_000_000))];
    assert!(pair_processes_to_sessions(&[], &files, &mut |_| None).is_empty());
    let procs = [a_proc(P1, 1_000_000)];
    let empty: [SessionFile; 0] = [];
    assert!(pair_processes_to_sessions(&procs, &empty, &mut |_| None).is_empty());
}

#[test]
fn a_process_with_no_readable_start_time_is_never_paired_by_timing() {
    // `lstart` that would not parse leaves the process pairable by identity
    // only — the alternative is arithmetic against the epoch.
    let procs = [ClaudeProcess {
        start: None,
        ..a_proc(P1, 0)
    }];
    let files = [a_file("fresh", 2_000_000, Some(1_000))];
    assert!(pair(&procs, &files).is_empty());
}

#[test]
fn the_same_input_always_produces_the_same_pairing() {
    // Two processes and two files with identical deltas: the tie is broken by
    // the order the caller handed them over, every time.
    const A: u32 = 21;
    const B: u32 = 22;
    let procs = [a_proc(A, 1_000), a_proc(B, 1_000)];
    let files = [
        a_file("x", 2_000_000, Some(2_000)),
        a_file("y", 1_000_000, Some(2_000)),
    ];
    let first = pair(&procs, &files);
    for _ in 0..20 {
        assert_eq!(pair(&procs, &files), first);
    }
    assert_eq!(first, vec![(A, "x".to_string()), (B, "y".to_string())]);
}
