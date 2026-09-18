//! The run model — what a run row says and in what order.
//!
//! The ordering rules are the feature. With seven passes in flight the board is
//! only useful if the row that needs the reviewer sorts to the top and the row
//! that is finished stops claiming to be busy. Both have an obvious wrong
//! implementation that looks right in a screenshot, so both are asserted here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::*;
use crate::qaden::{QaRunState, QaVerdict};
use crate::types::{
    Notification, NotificationKind, NotificationLevel, NotificationStatus, Session,
};

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// A `qa_run_state`-shaped record, without touching a disk.
fn state(open: usize, closed: usize) -> QaRunState {
    QaRunState {
        exists: true,
        round: 1,
        head: Some("abc1234".to_string()),
        current_head: None,
        stale: false,
        open_gaps: open,
        closed_gaps: closed,
        verdict: None,
        phase: Some("P2".to_string()),
        note_written: false,
        dir: PathBuf::from("/qa"),
        worktree: Some(PathBuf::from("/wt")),
    }
}

fn session_silent_for(silence: Duration) -> Session {
    Session {
        session_id: "sess".to_string(),
        pids: vec![4242],
        cwd: "/repo".to_string(),
        tty: Some("ttys004".to_string()),
        lstart: None,
        session_file: None,
        session_mtime: now() - silence,
        session_size: None,
        status: crate::types::SessionStatus::Working,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
        run_id: None,
    }
}

pub(crate) fn question() -> Notification {
    Notification {
        id: "q1".to_string(),
        title: "which environment?".to_string(),
        message: String::new(),
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: Some(1),
        level: NotificationLevel::Warn,
        kind: NotificationKind::Question,
        ts: String::new(),
        status: NotificationStatus::Unread,
    }
}

pub(crate) fn run(task_ids: Vec<i64>) -> QaRun {
    QaRun {
        id: QaRun::id_for("Project", "Quality Assurance"),
        project_name: "Project".to_string(),
        stage_name: "Quality Assurance".to_string(),
        task_ids,
        started_at: String::new(),
        lane_limit: None,
        coordinator_started: false,
        spawned: Vec::new(),
        mode: RunMode::Shadow,
    }
}

mod answer;
mod coordinator;
mod schedule;
mod shadow;

// ---------------------------------------------------------------- status ----

#[test]
fn a_task_nobody_has_started_is_queued() {
    assert_eq!(
        qa_task_status(None, None, false, Some(now())),
        QaStatus::Queued
    );
}

#[test]
fn an_outstanding_question_outranks_everything() {
    // Even a finished verdict. If the agent is asking, the run is stopped and
    // the reviewer is why — that must not hide behind a green tick.
    let mut finished = state(0, 9);
    finished.verdict = Some(QaVerdict::Pass);
    let live = session_silent_for(Duration::ZERO);
    assert_eq!(
        qa_task_status(Some(&finished), Some(&live), true, Some(now())),
        QaStatus::Asks
    );
}

#[test]
fn a_recorded_verdict_outranks_a_still_open_session() {
    // The trap this ordering exists for: /qa does NOT end its session after
    // parking a verdict — it prints the note and waits. Ranking "has a live
    // session" first leaves every completed pass reading "testing" forever.
    let live = session_silent_for(Duration::ZERO);

    let mut passed = state(0, 9);
    passed.verdict = Some(QaVerdict::Pass);
    assert_eq!(
        qa_task_status(Some(&passed), Some(&live), false, Some(now())),
        QaStatus::Pass
    );

    let mut failed = state(0, 9);
    failed.verdict = Some(QaVerdict::Revisions);
    assert_eq!(
        qa_task_status(Some(&failed), Some(&live), false, Some(now())),
        QaStatus::Revisions
    );
}

#[test]
fn a_silent_session_is_stalled_even_with_a_verdict_on_disk() {
    // A verdict from an earlier round plus a session that died mid-round must
    // not read as finished.
    let mut passed = state(0, 9);
    passed.verdict = Some(QaVerdict::Pass);
    let quiet = session_silent_for(STALL + Duration::from_secs(1));
    assert_eq!(
        qa_task_status(Some(&passed), Some(&quiet), false, Some(now())),
        QaStatus::Stalled
    );
}

#[test]
fn a_session_just_under_the_threshold_is_still_testing() {
    let nearly = session_silent_for(STALL - Duration::from_secs(1));
    assert_eq!(
        qa_task_status(Some(&state(3, 2)), Some(&nearly), false, Some(now())),
        QaStatus::Testing
    );
}

#[test]
fn a_transcript_stamped_in_the_future_is_not_silence() {
    // A clock change can stamp a file ahead of now. duration_since errs there,
    // and treating the error as "very stale" would flag a healthy session.
    let ahead = session_silent_for(Duration::ZERO);
    let earlier = now() - Duration::from_secs(60);
    assert_eq!(
        qa_task_status(Some(&state(1, 0)), Some(&ahead), false, Some(earlier)),
        QaStatus::Testing
    );
}

#[test]
fn gaps_on_disk_mean_testing_without_a_live_session() {
    assert_eq!(
        qa_task_status(Some(&state(3, 2)), None, false, Some(now())),
        QaStatus::Testing
    );
}

#[test]
fn a_run_file_that_holds_nothing_is_still_queued() {
    assert_eq!(
        qa_task_status(Some(&state(0, 0)), None, false, Some(now())),
        QaStatus::Queued
    );
}

// --------------------------------------------------------------- entries ----

#[test]
fn rows_sort_by_what_needs_you_most() {
    let live = session_silent_for(Duration::ZERO);
    let ask = question();

    let mut passed = state(0, 9);
    passed.verdict = Some(QaVerdict::Pass);

    let ctx = RunCtx {
        run_states: HashMap::from([
            (10, passed),
            (11, state(4, 5)),
            (12, state(0, 0)),
            (13, state(1, 0)),
        ]),
        sessions: HashMap::from([(11, &live), (13, &live)]),
        asks: HashMap::from([(13, &ask)]),
        now: Some(now()),
    };

    let order: Vec<_> = build_run_entries(&run(vec![10, 11, 12, 13]), &ctx)
        .iter()
        .map(|e| (e.task_id, e.status))
        .collect();
    assert_eq!(
        order,
        vec![
            (13, QaStatus::Asks),
            (10, QaStatus::Pass),
            (11, QaStatus::Testing),
            (12, QaStatus::Queued),
        ]
    );
}

#[test]
fn ties_break_on_task_id_so_rows_do_not_shuffle_under_the_cursor() {
    let ctx = RunCtx {
        now: Some(now()),
        ..RunCtx::default()
    };
    let ids: Vec<_> = build_run_entries(&run(vec![30, 10, 20]), &ctx)
        .iter()
        .map(|e| e.task_id)
        .collect();
    assert_eq!(ids, vec![10, 20, 30]);
}

#[test]
fn a_run_keeps_a_task_that_left_the_stage() {
    // The reason a run is a record and not a view. A task that fails QA moves
    // to a revision stage and is still part of the run that found the problem —
    // and "2 done of 7" needs a denominator that does not move.
    let ctx = RunCtx {
        now: Some(now()),
        ..RunCtx::default()
    };
    assert_eq!(build_run_entries(&run(vec![1, 2, 3, 4]), &ctx).len(), 4);
}

#[test]
fn an_empty_run_produces_no_rows() {
    let ctx = RunCtx::default();
    assert!(build_run_entries(&run(vec![]), &ctx).is_empty());
}

// --------------------------------------------------------------- summary ----

#[test]
fn the_summary_counts_both_verdicts_as_done() {
    let ctx = RunCtx {
        now: Some(now()),
        run_states: HashMap::from([
            (1, {
                let mut s = state(0, 1);
                s.verdict = Some(QaVerdict::Pass);
                s
            }),
            (2, {
                let mut s = state(0, 1);
                s.verdict = Some(QaVerdict::Revisions);
                s
            }),
            (3, state(1, 0)),
        ]),
        ..RunCtx::default()
    };
    let summary = run_summary(&build_run_entries(&run(vec![1, 2, 3]), &ctx));
    assert_eq!(summary.total, 3);
    assert_eq!(summary.done, 2);
    assert_eq!(summary.testing, 1);
    assert!(!summary.finished());
}

#[test]
fn an_empty_run_is_not_finished() {
    // Otherwise a run created a moment before its tasks load reads as complete.
    assert!(!RunSummary::default().finished());
}

#[test]
fn a_run_with_nothing_waiting_does_not_want_attention() {
    let summary = RunSummary {
        total: 2,
        testing: 2,
        ..RunSummary::default()
    };
    assert!(!summary.wants_attention());
}

// ---------------------------------------------------------------- asks ------

#[test]
fn pending_asks_are_flat_across_runs() {
    // With three runs open, "which run" is not the reviewer's question.
    let ask = question();
    let ctx = RunCtx {
        now: Some(now()),
        asks: HashMap::from([(1, &ask), (3, &ask)]),
        ..RunCtx::default()
    };
    let a = run(vec![1, 2]);
    let mut b = run(vec![3]);
    b.id = QaRun::id_for("Other", "QA");

    let found: Vec<_> = pending_asks(&[a, b], &ctx)
        .iter()
        .map(|p| p.task_id)
        .collect();
    assert_eq!(found, vec![1, 3]);
}

#[test]
fn a_stalled_task_counts_as_wanting_you() {
    let quiet = session_silent_for(STALL + Duration::from_secs(1));
    let ctx = RunCtx {
        now: Some(now()),
        sessions: HashMap::from([(1, &quiet)]),
        ..RunCtx::default()
    };
    let asks = pending_asks(&[run(vec![1])], &ctx);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].status, QaStatus::Stalled);
}

#[test]
fn the_jump_key_wraps_rather_than_sticking() {
    let ask = question();
    let ctx = RunCtx {
        now: Some(now()),
        asks: HashMap::from([(1, &ask), (2, &ask)]),
        ..RunCtx::default()
    };
    let runs = [run(vec![1, 2])];
    let id = &runs[0].id;

    let first = next_ask_key(&runs, &ctx, None).unwrap();
    assert_eq!(first, run_task_key(id, 1));

    let second = next_ask_key(&runs, &ctx, Some(&first)).unwrap();
    assert_eq!(second, run_task_key(id, 2));

    // …and back to the start, so pressing it repeatedly cycles.
    assert_eq!(next_ask_key(&runs, &ctx, Some(&second)).unwrap(), first);
}

#[test]
fn the_jump_key_answers_nothing_when_nothing_waits() {
    let ctx = RunCtx {
        now: Some(now()),
        ..RunCtx::default()
    };
    assert_eq!(next_ask_key(&[run(vec![1])], &ctx, None), None);
}

// ------------------------------------------------------------- rendering ----

#[test]
fn the_wide_cell_shows_gap_progress() {
    let entry = RunEntry {
        task_id: 1,
        status: QaStatus::Testing,
        run: None,
        session: None,
        ask: None,
    };
    let state = state(4, 5);
    let entry = RunEntry {
        run: Some(&state),
        ..entry
    };
    assert_eq!(qa_cell_text(&entry, true), "⠿ testing 5/9");
}

#[test]
fn the_narrow_cell_is_the_glyph_alone() {
    let entry = RunEntry {
        task_id: 1,
        status: QaStatus::Asks,
        run: None,
        session: None,
        ask: None,
    };
    assert_eq!(qa_cell_text(&entry, false), "⏸");
}

#[test]
fn an_ask_past_round_one_says_which_round() {
    // Which round it is changes what the answer should be.
    let mut later = state(1, 0);
    later.round = 3;
    let entry = RunEntry {
        task_id: 1,
        status: QaStatus::Asks,
        run: Some(&later),
        session: None,
        ask: None,
    };
    assert_eq!(qa_cell_text(&entry, true), "⏸ asks you r3");
}

#[test]
fn every_status_renders_without_a_run_record() {
    // A queued task has no run.json at all, and none of these may render empty.
    for status in [
        QaStatus::Asks,
        QaStatus::Stalled,
        QaStatus::Revisions,
        QaStatus::Pass,
        QaStatus::Testing,
        QaStatus::Queued,
    ] {
        let entry = RunEntry {
            task_id: 1,
            status,
            run: None,
            session: None,
            ask: None,
        };
        assert!(!qa_cell_text(&entry, true).is_empty());
        assert!(!qa_cell_text(&entry, false).is_empty());
    }
}

#[test]
fn the_header_carries_the_fixed_denominator() {
    let summary = RunSummary {
        total: 3,
        done: 1,
        testing: 1,
        queued: 1,
        ..RunSummary::default()
    };
    let wide = run_header_text(&run(vec![1, 2, 3]), &summary, true);
    assert!(wide.contains("QA RUN · Quality Assurance"), "{wide}");
    assert!(
        wide.contains("3 tasks · 1 done · 1 testing · 1 queued"),
        "{wide}"
    );
}

#[test]
fn the_header_drops_the_stage_name_on_a_narrow_pane() {
    // It is already on screen one row above.
    let summary = RunSummary {
        total: 3,
        done: 1,
        asking: 2,
        ..RunSummary::default()
    };
    let narrow = run_header_text(&run(vec![1, 2, 3]), &summary, false);
    assert!(narrow.contains("QA RUN"), "{narrow}");
    assert!(!narrow.contains("Quality Assurance"), "{narrow}");
    assert!(narrow.contains("2 ask"), "{narrow}");
}

#[test]
fn one_task_does_not_read_as_one_tasks() {
    let summary = RunSummary {
        total: 1,
        queued: 1,
        ..RunSummary::default()
    };
    assert!(run_header_text(&run(vec![1]), &summary, true).contains("1 task "));
}

#[test]
fn the_width_breakpoint_is_where_it_says_it_is() {
    assert!(is_wide(QA_WIDE_MIN_COLS));
    assert!(!is_wide(QA_WIDE_MIN_COLS - 1));
}
