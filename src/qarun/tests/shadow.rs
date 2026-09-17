//! The shadow record.
//!
//! One assertion carries the whole thing: a recorded answer cannot be changed.
//! Everything else is bookkeeping. If that refusal stops holding, the agreement
//! rate stops being evidence and becomes a number the coordinator can write for
//! itself — and the only reason to gather it was that no honest number existed.

use std::path::Path;

use crate::qarun::{ShadowError, ShadowStore};

const RUN: &str = "Project::Quality Assurance";

fn store(dir: &Path) -> ShadowStore {
    ShadowStore::new(dir)
}

fn at() -> String {
    "2026-09-17T12:00:00.000Z".to_string()
}

#[test]
fn a_record_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let path = store
        .record(
            RUN,
            1,
            "which environment?",
            "the MR preview",
            Some("high"),
            at(),
        )
        .unwrap();
    assert!(path.exists());

    let records = store.records(RUN);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].would_answer, "the MR preview");
    assert_eq!(records[0].confidence.as_deref(), Some("high"));
    // Fresh records are unscored, not scored as a disagreement.
    assert_eq!(records[0].agreed, None);
    assert_eq!(records[0].actual_answer, None);
}

#[test]
fn a_recorded_answer_cannot_be_overwritten() {
    // The ordering rule. A second opinion formed after seeing the real answer
    // is not a prediction, and an editable prediction measures nothing.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.record(RUN, 2, "q", "first", None, at()).unwrap();

    assert_eq!(
        store.record(RUN, 2, "q", "second", None, at()),
        Err(ShadowError::AlreadyRecorded(2))
    );
    assert_eq!(store.records(RUN)[0].would_answer, "first");
}

#[test]
fn an_empty_answer_is_refused_but_i_do_not_know_is_not() {
    // "I could not answer this" is a real data point — it is the case where
    // triage would have escalated anyway, and the rate has to count it.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    assert_eq!(
        store.record(RUN, 3, "q", "   ", None, at()),
        Err(ShadowError::EmptyAnswer)
    );
    assert!(store
        .record(
            RUN,
            3,
            "q",
            "I do not know: the task does not say.",
            None,
            at()
        )
        .is_ok());
}

#[test]
fn a_hostile_run_id_cannot_escape_the_shadow_directory() {
    // Run ids carry project and stage names, both free text out of Odoo. The
    // property is containment, not the absence of dots: a flattened name like
    // ".._.._etc_passwd" is a perfectly safe single directory.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let root = store.root().to_path_buf();

    for (n, run_id) in ["../../etc/passwd", "..", ".", "....", "/", "a/../../b"]
        .into_iter()
        .enumerate()
    {
        // A distinct task id per case: several of these collapse to the same
        // safe directory, and reusing one id would trip the overwrite guard
        // instead of testing containment.
        let path = store
            .record(run_id, 900 + n as i64, "q", "x", None, at())
            .expect("a hostile run id should be flattened, not refused");
        assert!(
            path.starts_with(&root),
            "{run_id:?} wrote outside the shadow directory: {}",
            path.display()
        );
    }
}

#[test]
fn the_real_answer_attaches_afterwards_without_rewriting_the_prediction() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.record(RUN, 10, "q", "predicted", None, at()).unwrap();
    store
        .score(RUN, 10, Some("something else"), Some(false))
        .unwrap();

    let record = &store.records(RUN)[0];
    assert_eq!(record.would_answer, "predicted");
    assert_eq!(record.actual_answer.as_deref(), Some("something else"));
    assert_eq!(record.agreed, Some(false));
}

#[test]
fn scoring_a_task_that_was_never_predicted_is_refused() {
    // Otherwise the rate can be improved by adding answers after the fact.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        store(dir.path()).score(RUN, 8888, Some("x"), Some(true)),
        Err(ShadowError::NotRecorded(8888))
    );
}

#[test]
fn an_unscored_run_reports_no_rate_rather_than_zero() {
    // A rate of 0% and "no data" are opposite conclusions, and a decision that
    // cannot tell them apart will be made on the wrong one.
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.record(RUN, 1, "q", "a", None, at()).unwrap();

    let agreement = store.agreement(RUN);
    assert_eq!(agreement.recorded, 1);
    assert_eq!(agreement.scored, 0);
    assert_eq!(agreement.rate(), None);
    assert!(agreement.summary().contains("no answers scored yet"));
}

#[test]
fn the_rate_counts_only_scored_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    for id in 1..=3 {
        store.record(RUN, id, "q", "a", None, at()).unwrap();
    }
    store.score(RUN, 1, Some("a"), Some(true)).unwrap();
    store.score(RUN, 2, Some("z"), Some(false)).unwrap();

    let agreement = store.agreement(RUN);
    assert_eq!(agreement.scored, 2);
    assert_eq!(agreement.agreed, 1);
    assert_eq!(agreement.rate(), Some(0.5));
    assert_eq!(agreement.unscored, 1);
}

#[test]
fn an_unknown_run_reports_nothing_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let agreement = store(dir.path()).agreement("never::existed");
    assert_eq!(agreement.recorded, 0);
    assert_eq!(agreement.rate(), None);
}
