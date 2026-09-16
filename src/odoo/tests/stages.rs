//! Stage resolution. The rule that matters is the last one: when nothing
//! matches, the answer is `None` and the task is left where it is. Advancing to
//! "the next column" could land a finished task in "Revision Required".

use serde_json::json;

use super::client_with;
use super::stub::Reply;
use crate::odoo::{resolve_stage, StageKind, StageRecord};

fn stages(names: &[&str]) -> Vec<StageRecord> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| StageRecord {
            id: index as i64 + 1,
            name: (*name).to_string(),
            sequence: index as i64 + 1,
        })
        .collect()
}

#[test]
fn a_configured_name_beats_every_default() {
    let list = stages(&["In Progress", "Quality Assurance", "Staging"]);
    let resolved = resolve_stage(&list, StageKind::Done, &["Staging".to_string()]).unwrap();
    assert_eq!(resolved.name, "Staging");
}

#[test]
fn the_configured_names_are_tried_in_order_and_skipped_when_absent() {
    let list = stages(&["In Progress", "Testing"]);
    let preferred = vec!["Not A Stage".to_string(), "Testing".to_string()];
    assert_eq!(
        resolve_stage(&list, StageKind::Done, &preferred)
            .unwrap()
            .name,
        "Testing"
    );
}

#[test]
fn an_empty_configured_name_is_ignored_rather_than_matching_nothing() {
    let list = stages(&["In Progress", "Quality Assurance"]);
    let resolved = resolve_stage(&list, StageKind::Done, &[String::new()]).unwrap();
    assert_eq!(resolved.name, "Quality Assurance");
}

#[test]
fn known_names_match_case_insensitively_and_in_priority_order() {
    let list = stages(&["testing", "quality assurance"]);
    // "Quality Assurance" outranks "Testing" in the default list, whatever the
    // column order in Odoo says.
    let resolved = resolve_stage(&list, StageKind::Done, &[]).unwrap();
    assert_eq!(resolved.name, "quality assurance");
    assert_eq!(resolved.id, 2);
}

#[test]
fn the_fuzzy_match_catches_stages_nobody_listed() {
    let list = stages(&["Backlog", "Client QA Review Queue"]);
    assert_eq!(
        resolve_stage(&list, StageKind::Done, &[]).unwrap().name,
        "Client QA Review Queue"
    );

    let list = stages(&["Backlog", "Currently In   Progress"]);
    assert_eq!(
        resolve_stage(&list, StageKind::InProgress, &[])
            .unwrap()
            .name,
        "Currently In   Progress"
    );
}

#[test]
fn fuzzy_qa_respects_word_boundaries() {
    // "Aquarium" contains "qa" only as part of another word.
    let list = stages(&["Aquarium duty", "Backlog"]);
    assert_eq!(resolve_stage(&list, StageKind::Done, &[]), None);
}

#[test]
fn doing_and_wip_are_working_stages() {
    assert_eq!(
        resolve_stage(&stages(&["Doing"]), StageKind::InProgress, &[])
            .unwrap()
            .name,
        "Doing"
    );
    assert_eq!(
        resolve_stage(&stages(&["Dev WIP"]), StageKind::InProgress, &[])
            .unwrap()
            .name,
        "Dev WIP"
    );
}

#[test]
fn nothing_matching_means_leave_the_task_alone() {
    // The motivating case: a project whose only forward column is the one for
    // QA-found bugs. Guessing lands finished work in it.
    let list = stages(&["Backlog", "Approved to Start", "Revision Required"]);
    assert_eq!(resolve_stage(&list, StageKind::Done, &[]), None);
    assert_eq!(resolve_stage(&list, StageKind::InProgress, &[]), None);
}

#[test]
fn a_project_with_no_stages_resolves_to_nothing() {
    assert_eq!(
        resolve_stage(&[], StageKind::Done, &["QA".to_string()]),
        None
    );
}

#[test]
fn the_two_directions_are_one_function_with_different_lists() {
    let list = stages(&["In Progress", "Quality Assurance"]);
    assert_eq!(
        resolve_stage(&list, StageKind::InProgress, &[])
            .unwrap()
            .name,
        "In Progress"
    );
    assert_eq!(
        resolve_stage(&list, StageKind::Done, &[]).unwrap().name,
        "Quality Assurance"
    );
}

#[test]
fn the_client_asks_odoo_for_one_project_s_stages_in_sequence_order() {
    let (client, server) = client_with(vec![Reply::result(json!([
        { "id": 1, "name": "Approved to Start", "sequence": 1 },
        { "id": 3, "name": "Quality Assurance", "sequence": 3 },
    ]))]);

    let resolved = client
        .resolve_stage_for_project(3, StageKind::Done, &[])
        .unwrap()
        .unwrap();
    assert_eq!(resolved.id, 3);
    assert_eq!(server.args(1)[5][0], json!([["project_ids", "in", [3]]]));
    assert_eq!(
        server.requests()[1]["params"]["args"][6]["order"],
        json!("sequence")
    );
}
