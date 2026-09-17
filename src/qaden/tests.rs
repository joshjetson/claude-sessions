//! Ported from the Node app's `test/qaden.test.js`.
//!
//! The QA menu label is the only thing standing between the reviewer and
//! picking the wrong QAden operation. The states look similar and behave very
//! differently, and neither is visible from the board, so the label has to say
//! which one will happen.
//!
//! The finished-round case is here because the first version of this code got
//! it wrong: it offered to "resume round 1" on five real runs that each had 7-9
//! gaps closed and none open.

use serde_json::json;

use super::*;

struct Fixture {
    _tmp: tempfile::TempDir,
    paths: Paths,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    Fixture { _tmp: tmp, paths }
}

struct Run {
    head: Option<&'static str>,
    closed: usize,
    open: usize,
    rounds: usize,
    worktree: Option<PathBuf>,
}

impl Default for Run {
    fn default() -> Self {
        Run {
            head: Some("abc1234"),
            closed: 0,
            open: 0,
            rounds: 0,
            worktree: None,
        }
    }
}

/// Write a `run.json` the way QAden's `matrix.py` shapes it.
fn write_run(paths: &Paths, task_id: i64, run: Run) -> PathBuf {
    let dir = paths.qa_task_dir(task_id);
    fs::create_dir_all(&dir).unwrap();
    let mut cells = serde_json::Map::new();
    for i in 0..run.closed {
        cells.insert(format!("G{i}:C1"), json!({ "verdict": "PASS" }));
    }
    for i in 0..run.open {
        cells.insert(format!("O{i}:C1"), json!({ "verdict": null }));
    }
    let mut meta = serde_json::Map::new();
    if let Some(head) = run.head {
        meta.insert("head".into(), json!(head));
    }
    if let Some(worktree) = &run.worktree {
        meta.insert("worktree".into(), json!(worktree.display().to_string()));
    }
    let body = json!({
        "schema": 1, "task": task_id, "phase": "P2",
        "rows": [], "columns": [], "cells": cells,
        "meta": meta, "findings": [],
    });
    fs::write(dir.join("run.json"), body.to_string()).unwrap();
    for i in 1..=run.rounds {
        fs::write(dir.join(format!("run-round{i}.json")), "{}").unwrap();
    }
    dir
}

fn state(paths: &Paths, task_id: i64) -> QaRunState {
    qa_run_state(paths, task_id, |_| None)
}

// --- qa_run_state -----------------------------------------------------------

#[test]
fn a_task_qaden_has_never_seen_reports_no_run() {
    let f = fixture();
    let s = state(&f.paths, 700001);
    assert!(!s.exists);
    assert_eq!(s.round, 0);
    assert!(s.dir.ends_with("task-700001-qa"));
}

#[test]
fn unreadable_json_is_treated_as_no_run_not_an_error() {
    let f = fixture();
    let dir = f.paths.qa_task_dir(700002);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run.json"), "{ this is not json").unwrap();
    assert!(!state(&f.paths, 700002).exists);
}

#[test]
fn the_round_number_counts_archived_rounds_plus_the_live_one() {
    let f = fixture();
    write_run(
        &f.paths,
        700003,
        Run {
            rounds: 2,
            closed: 3,
            ..Run::default()
        },
    );
    assert_eq!(state(&f.paths, 700003).round, 3);
}

#[test]
fn files_that_are_not_archived_rounds_do_not_count() {
    let f = fixture();
    let dir = write_run(
        &f.paths,
        700031,
        Run {
            rounds: 1,
            ..Run::default()
        },
    );
    for stray in ["run-roundX.json", "run-round.json", "run-round2.json.bak"] {
        fs::write(dir.join(stray), "{}").unwrap();
    }
    assert_eq!(state(&f.paths, 700031).round, 2);
}

#[test]
fn gaps_are_split_into_open_and_closed() {
    let f = fixture();
    write_run(
        &f.paths,
        700004,
        Run {
            closed: 4,
            open: 2,
            ..Run::default()
        },
    );
    let s = state(&f.paths, 700004);
    assert_eq!(s.closed_gaps, 4);
    assert_eq!(s.open_gaps, 2);
}

#[test]
fn staleness_is_not_claimed_when_the_worktree_is_gone() {
    // The common case after a worktree clean: a recorded head, nothing to
    // compare it against. Guessing "stale" would push the reviewer into
    // archiving a round that may still be current.
    let f = fixture();
    write_run(
        &f.paths,
        700005,
        Run {
            head: Some("deadbee"),
            worktree: Some(f.paths.qa_root.join("no-such-worktree")),
            ..Run::default()
        },
    );
    let s = state(&f.paths, 700005);
    assert_eq!(s.current_head, None);
    assert!(!s.stale);
}

#[test]
fn staleness_needs_both_commits_to_be_known() {
    let f = fixture();
    let worktree = f.paths.home.join("repo");
    fs::create_dir_all(&worktree).unwrap();
    write_run(
        &f.paths,
        700006,
        Run {
            head: Some("aaa1111"),
            worktree: Some(worktree.clone()),
            closed: 2,
            ..Run::default()
        },
    );
    // Same commit: not stale.
    let same = qa_run_state(&f.paths, 700006, |_| Some("aaa1111".to_string()));
    assert!(!same.stale);
    // Different commit: stale.
    let moved = qa_run_state(&f.paths, 700006, |_| Some("bbb2222".to_string()));
    assert!(moved.stale);
    // No recorded head at all: nothing to compare.
    write_run(
        &f.paths,
        700007,
        Run {
            head: None,
            worktree: Some(worktree),
            ..Run::default()
        },
    );
    assert!(!qa_run_state(&f.paths, 700007, |_| Some("bbb2222".into())).stale);
}

// --- qa_menu_label ----------------------------------------------------------

#[test]
fn offers_a_plain_start_when_there_is_no_prior_run() {
    let f = fixture();
    assert_eq!(qa_menu_label(&state(&f.paths, 700010)), "🧪  QA this task");
}

#[test]
fn offers_the_next_round_when_every_gap_is_closed() {
    let f = fixture();
    write_run(
        &f.paths,
        700011,
        Run {
            closed: 9,
            ..Run::default()
        },
    );
    assert!(qa_menu_label(&state(&f.paths, 700011)).contains("start round 2 (round 1 complete)"));
}

#[test]
fn offers_to_resume_while_gaps_are_still_open_and_says_how_many() {
    let f = fixture();
    write_run(
        &f.paths,
        700012,
        Run {
            closed: 2,
            open: 3,
            ..Run::default()
        },
    );
    assert!(qa_menu_label(&state(&f.paths, 700012)).contains("resume round 1, 3 gaps open"));
}

#[test]
fn singular_gap_reads_as_a_gap_not_one_gaps() {
    let f = fixture();
    write_run(
        &f.paths,
        700013,
        Run {
            open: 1,
            ..Run::default()
        },
    );
    assert!(qa_menu_label(&state(&f.paths, 700013)).contains("1 gap open"));
}

#[test]
fn a_completed_round_still_labels_as_complete_never_as_resumable() {
    // Guards the regression this module shipped with.
    let f = fixture();
    write_run(
        &f.paths,
        700014,
        Run {
            closed: 7,
            ..Run::default()
        },
    );
    let label = qa_menu_label(&state(&f.paths, 700014));
    assert!(!label.contains("resume"), "{label}");
    assert!(label.contains("start round 2"), "{label}");
}

#[test]
fn a_changed_commit_beats_both_the_complete_and_the_resume_wording() {
    let f = fixture();
    let worktree = f.paths.home.join("repo2");
    fs::create_dir_all(&worktree).unwrap();
    write_run(
        &f.paths,
        700015,
        Run {
            head: Some("aaa1111"),
            worktree: Some(worktree),
            closed: 3,
            open: 2,
            rounds: 1,
        },
    );
    let s = qa_run_state(&f.paths, 700015, |_| Some("bbb2222".to_string()));
    let label = qa_menu_label(&s);
    assert!(
        label.contains("start round 3 (code changed since round 2)"),
        "{label}"
    );
}

#[test]
fn a_run_with_no_cells_at_all_offers_a_plain_resume() {
    let f = fixture();
    write_run(&f.paths, 700016, Run::default());
    assert_eq!(
        qa_menu_label(&state(&f.paths, 700016)),
        "🧪  QA — resume round 1"
    );
}

// --- the head cache ---------------------------------------------------------

#[test]
fn the_cache_answers_nothing_until_it_has_been_filled() {
    // The render path must never learn a head by spawning git.
    let cache = HeadCache::new();
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(cache.cached(tmp.path()), None);
}

#[test]
fn a_refusing_spawn_policy_caches_a_miss_rather_than_running_git() {
    let cache = HeadCache::new();
    let exec = Exec::new(crate::term::SpawnPolicy::Refuse);
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(cache.refresh(&exec, tmp.path()), None);
    assert_eq!(cache.cached(tmp.path()), None);
}

#[test]
fn menu_label_for_reads_the_disk_without_any_head() {
    let f = fixture();
    write_run(
        &f.paths,
        700020,
        Run {
            open: 2,
            ..Run::default()
        },
    );
    let label = menu_label_for(&f.paths, 700020, &HeadCache::new());
    assert!(label.contains("resume round 1, 2 gaps open"), "{label}");
}
