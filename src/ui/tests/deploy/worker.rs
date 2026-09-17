//! Merging and loading, off the draw thread.

use std::sync::mpsc::channel;

use super::PROJECT;
use crate::gitlab::tests::{client as gitlab_client, failed, json_ok, ok};
use crate::paths::Paths;
use crate::ui::actions::{deploy, ActionResult, BoardData, BoardServices};
use crate::ui::state::MergeTarget;

fn services(commands: Vec<crate::term::CommandOutput>) -> (tempfile::TempDir, BoardServices) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut services = BoardServices::offline(Paths::for_test(dir.path()));
    let (gitlab, _runner) = gitlab_client(commands);
    services.gitlab = gitlab;
    (dir, services)
}

fn target(iid: i64) -> MergeTarget {
    MergeTarget {
        task_id: iid - 100,
        name: format!("Task {}", iid - 100),
        iid,
        project_path: "group/repo".to_string(),
    }
}

fn flashes(rx: &std::sync::mpsc::Receiver<ActionResult>) -> Vec<String> {
    rx.try_iter()
        .filter_map(|result| match result {
            ActionResult::Flash(message) => Some(message),
            _ => None,
        })
        .collect()
}

#[test]
fn merging_reports_progress_one_at_a_time_and_then_the_total() {
    let (_dir, services) = services(vec![ok("Merged")]);
    let (tx, rx) = channel();
    deploy::merge(&services, PROJECT, &[target(101), target(102)], &tx);
    let said = flashes(&rx);
    // Progress as it goes, so a slow batch does not look hung.
    assert!(said.iter().any(|m| m.contains("[1/2]")), "{said:?}");
    assert!(said.iter().any(|m| m.contains("[2/2]")), "{said:?}");
    assert!(
        said.iter().any(|m| m.contains("✓ Merged 2 MRs in Aurora")),
        "{said:?}"
    );
}

#[test]
fn a_failure_names_the_merge_request_and_gitlabs_own_reason() {
    // The whole point of merging sequentially: eight parallel merges with
    // one failure leave the user guessing which one did not happen.
    let (_dir, services) = services(vec![ok("Merged"), failed("! pipeline must succeed")]);
    let (tx, rx) = channel();
    deploy::merge(&services, PROJECT, &[target(101), target(102)], &tx);
    let said = flashes(&rx);
    let summary = said.last().cloned().unwrap_or_default();
    assert!(summary.contains("Merged 1/2"), "{summary}");
    assert!(
        summary.contains("!102: ! pipeline must succeed"),
        "{summary}"
    );
}

#[test]
fn merging_one_names_the_task_it_belongs_to() {
    let (_dir, services) = services(vec![ok("Merged")]);
    let (tx, rx) = channel();
    deploy::merge(&services, PROJECT, &[target(101)], &tx);
    let summary = flashes(&rx)
        .into_iter()
        .find(|m| m.starts_with('✓'))
        .unwrap_or_default();
    assert!(summary.contains("#1 Task 1"), "{summary}");
}

#[test]
fn merging_nothing_says_so_and_runs_no_command() {
    let (_dir, services) = services(vec![ok("")]);
    let (tx, rx) = channel();
    deploy::merge(&services, PROJECT, &[], &tx);
    assert_eq!(flashes(&rx), ["No MRs ready to merge in Aurora."]);
}

#[test]
fn a_board_refresh_with_no_odoo_says_why_rather_than_looking_empty() {
    let (_dir, services) = services(vec![ok("")]);
    let (tx, rx) = channel();
    deploy::refresh(&services, &tx);
    let Some(ActionResult::Deploy(update)) = rx.try_iter().next() else {
        panic!("no deploy update");
    };
    assert!(update.error.unwrap().contains("No Odoo credentials"));
}

#[test]
fn the_open_mr_lookup_hands_its_answer_to_whichever_dialog_asked() {
    let (_dir, services) = services(vec![json_ok(serde_json::json!([{
        "iid": 403,
        "title": "Widget rollout",
        "web_url": "https://git.example.com/group/repo/-/merge_requests/403",
    }]))]);
    let (tx, rx) = channel();
    deploy::open_mrs(&services, &tx);
    match rx.try_iter().next() {
        Some(ActionResult::Data(data)) => match *data {
            BoardData::OpenMrs(mrs) => assert_eq!(mrs.len(), 1),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_gitlab_failure_reaches_the_dialog_as_a_failure_not_an_empty_list() {
    let (_dir, services) = services(vec![failed("401 Unauthorized")]);
    let (tx, rx) = channel();
    deploy::open_mrs(&services, &tx);
    match rx.try_iter().next() {
        Some(ActionResult::Data(data)) => match *data {
            BoardData::Failed { task_id, error } => {
                assert_eq!(task_id, None);
                assert_eq!(error, "401 Unauthorized");
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}
