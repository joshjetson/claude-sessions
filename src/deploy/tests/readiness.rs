//! The blocker-reason matrix. `mergeReadiness` returning a SPECIFIC reason is
//! the whole value of the Deploy tab's merge row: "cannot merge" on its own
//! sends the user to GitLab to find out what the dashboard already knows.

use super::{open_mr, task_with_mr};
use crate::deploy::{has_conflicts, has_shipped, merge_readiness, state_label, FINISHED_STATES};
use crate::odoo::task_state;
use crate::types::{DeployTask, MergeRequest};

fn reason(task: &DeployTask) -> String {
    merge_readiness(task).reason
}

#[test]
fn a_clean_open_mr_is_ready_with_no_reason() {
    let task = task_with_mr(open_mr());
    let readiness = merge_readiness(&task);
    assert!(readiness.ready);
    assert_eq!(readiness.reason, "");
}

#[test]
fn every_blocker_names_itself() {
    let cases: Vec<(&str, DeployTask)> = vec![
        ("no merge request linked", DeployTask::default()),
        (
            "MR linked but its GitLab project is unknown",
            DeployTask {
                mr_iid: Some(403),
                ..DeployTask::default()
            },
        ),
        (
            "MR status not loaded yet",
            DeployTask {
                mr: None,
                ..task_with_mr(open_mr())
            },
        ),
        (
            "MR status unavailable (glab: command not found)",
            DeployTask {
                mr: None,
                mr_error: Some("glab: command not found".to_string()),
                ..task_with_mr(open_mr())
            },
        ),
        (
            "already merged",
            task_with_mr(MergeRequest {
                state: "merged".to_string(),
                ..open_mr()
            }),
        ),
        (
            "MR is closed",
            task_with_mr(MergeRequest {
                state: "closed".to_string(),
                ..open_mr()
            }),
        ),
        (
            "MR is a draft",
            task_with_mr(MergeRequest {
                draft: true,
                ..open_mr()
            }),
        ),
        (
            "has merge conflicts",
            task_with_mr(MergeRequest {
                conflicts: true,
                ..open_mr()
            }),
        ),
        (
            // GitLab's own word for it, underscores unpicked for reading.
            "GitLab says: ci still running",
            task_with_mr(MergeRequest {
                merge_status: "ci_still_running".to_string(),
                ..open_mr()
            }),
        ),
        (
            "GitLab says: not approved",
            task_with_mr(MergeRequest {
                merge_status: "not_approved".to_string(),
                ..open_mr()
            }),
        ),
    ];
    for (expected, task) in cases {
        let readiness = merge_readiness(&task);
        assert!(!readiness.ready, "{expected} should block the merge");
        assert_eq!(readiness.reason, expected);
    }
}

#[test]
fn the_most_fundamental_blocker_is_the_one_reported() {
    // A draft MR with conflicts and a failing pipeline is reported as a draft:
    // the order runs from "there is nothing to merge" outwards, so the user is
    // told the thing they have to fix first.
    let task = task_with_mr(MergeRequest {
        draft: true,
        conflicts: true,
        merge_status: "ci_still_running".to_string(),
        ..open_mr()
    });
    assert_eq!(reason(&task), "MR is a draft");

    // And a missing project path beats an unreadable MR: without the path
    // there is nothing to address a read to in the first place.
    let task = DeployTask {
        mr_project_path: String::new(),
        mr_error: Some("no access".to_string()),
        ..task_with_mr(open_mr())
    };
    assert_eq!(reason(&task), "MR linked but its GitLab project is unknown");
}

#[test]
fn an_empty_merge_status_does_not_block() {
    // Some self-managed servers send neither status field. Treating "" as a
    // blocker would make every MR on those servers permanently unmergeable.
    let task = task_with_mr(MergeRequest {
        merge_status: String::new(),
        ..open_mr()
    });
    assert!(merge_readiness(&task).ready);
}

#[test]
fn conflicts_are_detected_from_either_signal_but_only_while_the_mr_is_open() {
    let flagged = task_with_mr(MergeRequest {
        conflicts: true,
        ..open_mr()
    });
    assert!(has_conflicts(&flagged));

    // GitLab words it rather than flagging it on older servers.
    let worded = task_with_mr(MergeRequest {
        merge_status: "CONFLICT".to_string(),
        ..open_mr()
    });
    assert!(has_conflicts(&worded), "match is case-insensitive");

    for state in ["merged", "closed", "locked"] {
        let closed = task_with_mr(MergeRequest {
            state: state.to_string(),
            conflicts: true,
            ..open_mr()
        });
        assert!(
            !has_conflicts(&closed),
            "a {state} MR's conflict flag is not work for anybody"
        );
    }

    assert!(
        !has_conflicts(&DeployTask::default()),
        "no MR, no conflicts"
    );
}

#[test]
fn shipped_means_merged_and_nothing_weaker() {
    assert!(has_shipped(&task_with_mr(MergeRequest {
        state: "merged".to_string(),
        ..open_mr()
    })));
    for state in ["opened", "closed", "locked", ""] {
        assert!(
            !has_shipped(&task_with_mr(MergeRequest {
                state: state.to_string(),
                ..open_mr()
            })),
            "{state} did not ship"
        );
    }
    assert!(!has_shipped(&DeployTask::default()));
}

#[test]
fn the_finished_states_are_the_three_that_need_nothing_further() {
    assert_eq!(
        FINISHED_STATES,
        [
            task_state::COMPLETE,
            task_state::DONE,
            task_state::CANCELLED
        ]
    );
    // Deliberately NOT here: a task can sit in Deployed while its state is
    // still In Progress or Changes Requested, and those are the rows the tab
    // exists to show.
    assert!(!FINISHED_STATES.contains(&task_state::IN_PROGRESS));
    assert!(!FINISHED_STATES.contains(&task_state::CHANGES_REQUESTED));
}

#[test]
fn state_labels_cover_every_state_and_never_show_a_raw_key_as_blank() {
    assert_eq!(state_label(task_state::COMPLETE), "Complete");
    assert_eq!(state_label(task_state::DONE), "Done");
    assert_eq!(state_label(task_state::IN_PROGRESS), "In Progress");
    assert_eq!(
        state_label(task_state::CHANGES_REQUESTED),
        "Changes Requested"
    );
    assert_eq!(state_label(task_state::WAITING), "Waiting");
    assert_eq!(state_label(task_state::CANCELLED), "Cancelled");
    assert_eq!(state_label(""), "Unknown");
    // An unseen key shows itself rather than vanishing.
    assert_eq!(state_label("05_new_thing"), "05_new_thing");
}
