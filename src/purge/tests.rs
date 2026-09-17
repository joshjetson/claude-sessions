//! Ported from the Node app's `test/purge.test.js`.
//!
//! The whole point of the feature is to clear finished work off the tab bar, so
//! the interesting cases are the ones it must NOT touch: an agent still
//! working, a task heading back for revision, a session nobody can attribute to
//! a task, and a stage name from a project nobody anticipated.

use std::collections::HashMap;

use super::*;

fn target(task_id: Option<i64>) -> PurgeTarget {
    PurgeTarget {
        session_id: match task_id {
            Some(id) => format!("s-{id}"),
            None => "plain".to_string(),
        },
        pids: vec![1],
        tty: Some("ttys001".to_string()),
        task_id,
    }
}

/// A stage lookup built from pairs, the way the dialog builds it from the board.
fn stages(pairs: &[(i64, &str)]) -> impl Fn(i64) -> Option<String> {
    let map: HashMap<i64, String> = pairs
        .iter()
        .map(|(id, name)| (*id, (*name).to_string()))
        .collect();
    move |id| map.get(&id).cloned()
}

// --- classify_stage ---------------------------------------------------------

#[test]
fn the_working_stages_on_the_real_board() {
    for stage in ["In Progress", "Approved to Start", "Revision Required"] {
        assert_eq!(
            classify_stage(stage),
            Stage::Working,
            "{stage} must never be purged"
        );
    }
}

#[test]
fn the_review_stages_in_both_spellings_each_project_uses() {
    // "QA" in one project is "Quality Assurance" in another; likewise UAT.
    for stage in [
        "QA",
        "Quality Assurance",
        "UAT",
        "User Acceptance Testing",
        "Staging",
    ] {
        assert_eq!(
            classify_stage(stage),
            Stage::Review,
            "{stage} should be the toggleable bucket"
        );
    }
}

#[test]
fn finished_stages() {
    for stage in [
        "Deployed",
        "Done",
        "Completed",
        "Closed",
        "Merged",
        "Cancelled",
    ] {
        assert_eq!(
            classify_stage(stage),
            Stage::Done,
            "{stage} should be purgeable"
        );
    }
}

#[test]
fn anything_unrecognised_is_unknown_not_not_working() {
    // Backlog and Resources are real stages with no business being swept up.
    for stage in ["Backlog", "Resources", "Waiting on client", ""] {
        assert_eq!(classify_stage(stage), Stage::Unknown, "{stage:?}");
    }
}

#[test]
fn naming_is_case_and_spacing_insensitive() {
    assert_eq!(classify_stage("  in   progress "), Stage::Working);
    assert_eq!(classify_stage("DEPLOYED"), Stage::Done);
}

#[test]
fn word_boundaries_are_respected() {
    // "\bcomplete(d)?\b" must not fire on "incomplete" or "completely".
    assert_eq!(classify_stage("Incomplete"), Stage::Unknown);
    assert_eq!(classify_stage("Completely blocked"), Stage::Unknown);
    // …but the whole word still matches inside a longer stage name.
    assert_eq!(classify_stage("Deployed to prod"), Stage::Done);
}

// --- plan_purge -------------------------------------------------------------

#[test]
fn the_board_as_it_stands_finished_go_live_stay() {
    let live: Vec<PurgeTarget> = [6440, 6608, 6611, 4033, 6590, 9001]
        .into_iter()
        .map(|id| target(Some(id)))
        .collect();
    let plan = plan_purge(
        &live,
        stages(&[
            (6440, "QA"),
            (6608, "QA"),
            (6611, "UAT"),
            (4033, "In Progress"),
            (6590, "Revision Required"),
            (9001, "Deployed"),
        ]),
        false,
    );
    assert_eq!(
        plan.purge
            .iter()
            .map(|e| e.target.task_id)
            .collect::<Vec<_>>(),
        vec![Some(9001)]
    );
    assert_eq!(plan.keep.len(), 5);
}

#[test]
fn the_qa_toggle_takes_review_stages_too() {
    let live: Vec<PurgeTarget> = [6440, 6611, 4033]
        .into_iter()
        .map(|id| target(Some(id)))
        .collect();
    let stage_of = stages(&[(6440, "QA"), (6611, "UAT"), (4033, "In Progress")]);
    let off = plan_purge(&live, &stage_of, false);
    let on = plan_purge(&live, &stage_of, true);
    assert!(off.purge.is_empty());
    let mut purged: Vec<i64> = on.purge.iter().filter_map(|e| e.target.task_id).collect();
    purged.sort_unstable();
    assert_eq!(purged, vec![6440, 6611]);
    // Never at the cost of live work.
    assert!(on.keep.iter().any(|k| k.target.task_id == Some(4033)));
}

#[test]
fn a_session_with_no_task_is_never_purged_whatever_the_toggle() {
    // The dashboard's own session is one of these, as is anything hand-started.
    let live = vec![target(None)];
    for include_review in [false, true] {
        let plan = plan_purge(&live, stages(&[]), include_review);
        assert!(plan.purge.is_empty());
        assert_eq!(plan.keep[0].reason, KeepReason::NoTask);
    }
}

#[test]
fn an_unresolvable_stage_is_kept_not_read_as_finished() {
    // Absence of evidence: the board is filtered, and Odoo may be unreachable.
    // Guessing "finished" here would kill somebody else's live agent.
    let plan = plan_purge(&[target(Some(7777))], |_| None, false);
    assert!(plan.purge.is_empty());
    assert_eq!(plan.keep[0].reason, KeepReason::StageUnknown);
}

#[test]
fn an_unrecognised_stage_is_kept_and_named_so_it_can_be_asked_about() {
    let plan = plan_purge(
        &[target(Some(42))],
        stages(&[(42, "Waiting on client")]),
        false,
    );
    assert!(plan.purge.is_empty());
    assert_eq!(plan.keep[0].stage.as_deref(), Some("Waiting on client"));
    assert_eq!(plan.keep[0].reason, KeepReason::UnrecognisedStage);
    assert!(plan.keep[0].reason.label().contains("unrecognised"));
}

#[test]
fn every_session_lands_in_exactly_one_bucket() {
    let live: Vec<PurgeTarget> = [Some(1), Some(2), Some(3), None, Some(5)]
        .into_iter()
        .map(target)
        .collect();
    let plan = plan_purge(
        &live,
        stages(&[
            (1, "QA"),
            (2, "Deployed"),
            (3, "In Progress"),
            (5, "Nonsense"),
        ]),
        true,
    );
    assert_eq!(plan.purge.len() + plan.keep.len(), live.len());
    let ids: std::collections::HashSet<&str> = plan
        .purge
        .iter()
        .map(|e| e.target.session_id.as_str())
        .chain(plan.keep.iter().map(|e| e.target.session_id.as_str()))
        .collect();
    assert_eq!(ids.len(), live.len(), "a session was counted twice");
}

#[test]
fn counts_describe_what_will_happen() {
    let live: Vec<PurgeTarget> = [1, 2, 3].into_iter().map(|id| target(Some(id))).collect();
    let plan = plan_purge(
        &live,
        stages(&[(1, "QA"), (2, "Deployed"), (3, "In Progress")]),
        true,
    );
    let counts = plan.counts();
    assert_eq!(counts.purge, 2);
    assert_eq!(counts.review, 1);
    assert_eq!(counts.done, 1);
    assert_eq!(counts.keep, 1);
}

#[test]
fn no_sessions_at_all_is_not_an_error() {
    let plan = plan_purge(&[], stages(&[]), false);
    assert!(plan.purge.is_empty());
    assert!(plan.keep.is_empty());
}

// --- grouping ---------------------------------------------------------------

#[test]
fn groups_for_display_biggest_first() {
    let live: Vec<PurgeTarget> = [1, 2, 3].into_iter().map(|id| target(Some(id))).collect();
    let plan = plan_purge(
        &live,
        stages(&[(1, "QA"), (2, "QA"), (3, "Deployed")]),
        true,
    );
    let groups = group_purged(&plan.purge);
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.stage.as_str(), g.entries.len()))
            .collect::<Vec<_>>(),
        vec![("QA", 2), ("Deployed", 1)]
    );
}

#[test]
fn sessions_with_no_task_are_labelled_rather_than_blank() {
    let plan = plan_purge(&[target(None)], stages(&[]), false);
    assert_eq!(group_kept(&plan.keep)[0].stage, "No task");
}
