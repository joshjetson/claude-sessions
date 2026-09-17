//! The four irreversible confirmations: merge, merge all, deploy, and
//! handing a conflicted merge request to an agent.

use crossterm::event::KeyCode;

use super::{deploy_state, dialog_text, handle, mr, press, press_shift, select, task, PROJECT};
use crate::daemon::{TaskLink, TaskLinkStatus};
use crate::types::{DeployBoard, DeployProjectState, DeployTask, MergeRequest};
use crate::ui::board::BoardUpdate;
use crate::ui::deploy::DeployUpdate;
use crate::ui::dialogs::{DialogOutcome, MERGE_ALL_LISTED};
use crate::ui::state::{Action, View};

// --- dialogs --------------------------------------------------------------------------

#[test]
fn the_merge_confirmation_spells_out_source_target_and_pipeline() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('m'));
    let painted = dialog_text(&mut state);
    assert!(painted.contains("!101"), "{painted}");
    assert!(painted.contains("task-101-work"), "{painted}");
    assert!(painted.contains("development"), "{painted}");
    assert!(painted.contains("pipeline: success"), "{painted}");
    assert!(
        painted.contains("This merges the MR in GitLab now."),
        "{painted}"
    );
    // Cancel is first, so Enter-mashing does not merge to production.
    assert!(painted.contains("Cancel"), "{painted}");
}

#[test]
fn enter_on_a_merge_confirmation_cancels_and_the_second_option_merges() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('m'));
    assert_eq!(handle(&mut state, KeyCode::Enter), DialogOutcome::Close);

    press(&mut state, KeyCode::Char('m'));
    handle(&mut state, KeyCode::Right);
    let outcome = handle(&mut state, KeyCode::Enter);
    match outcome {
        DialogOutcome::Act(Action::MergeMrs { project, targets }) => {
            assert_eq!(project, PROJECT);
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].iid, 101);
            assert_eq!(targets[0].project_path, "group/repo");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn merge_all_lists_up_to_ten_and_counts_the_rest() {
    let tasks: Vec<DeployTask> = (1..=13).map(|i| task(i, Some(mr(100 + i)))).collect();
    let (_dir, mut state) = deploy_state(tasks);
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'M');
    let painted = dialog_text(&mut state);
    assert!(
        painted.contains("Merging 13 merge requests, one at a time"),
        "{painted}"
    );
    assert!(painted.contains("!101"), "{painted}");
    assert!(
        painted.contains(&format!("!{}", 100 + MERGE_ALL_LISTED as i64)),
        "the tenth must be listed: {painted}"
    );
    assert!(
        !painted.contains("!111"),
        "the eleventh must not be listed: {painted}"
    );
    assert!(painted.contains("…and 3 more"), "{painted}");
}

#[test]
fn merge_all_counts_the_blocked_ones_it_is_skipping() {
    let (_dir, mut state) = deploy_state(vec![
        task(1, Some(mr(101))),
        task(
            2,
            Some(MergeRequest {
                conflicts: true,
                ..mr(102)
            }),
        ),
        task(
            3,
            Some(MergeRequest {
                draft: true,
                ..mr(103)
            }),
        ),
        // Already merged: not ready, but not "blocked" either — nothing is in
        // its way, so counting it would overstate the problem.
        task(
            4,
            Some(MergeRequest {
                state: "merged".to_string(),
                ..mr(104)
            }),
        ),
        // No merge request at all: also not a blocked MR.
        task(5, None),
    ]);
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'M');
    let painted = dialog_text(&mut state);
    assert!(
        painted.contains("Merging 1 merge request, one at a time"),
        "{painted}"
    );
    assert!(
        painted.contains("2 other MRs still blocked — skipped."),
        "{painted}"
    );
}

#[test]
fn merge_all_with_nothing_ready_offers_only_a_way_out() {
    let (_dir, mut state) = deploy_state(vec![task(1, None)]);
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'M');
    assert!(dialog_text(&mut state).contains("No merge requests are ready to merge."));
    assert_eq!(handle(&mut state, KeyCode::Enter), DialogOutcome::Close);
}

#[test]
fn merge_all_enqueues_every_ready_target_in_board_order() {
    let (_dir, mut state) = deploy_state(vec![
        task(1, Some(mr(101))),
        task(2, None),
        task(3, Some(mr(103))),
    ]);
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'M');
    handle(&mut state, KeyCode::Right);
    match handle(&mut state, KeyCode::Enter) {
        DialogOutcome::Act(Action::MergeMrs { targets, .. }) => {
            assert_eq!(
                targets.iter().map(|t| t.iid).collect::<Vec<_>>(),
                vec![101, 103]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_deploy_confirmation_shows_the_literal_command_and_what_will_not_ship() {
    let (_dir, mut state) = deploy_state(vec![
        task(
            1,
            Some(MergeRequest {
                state: "merged".to_string(),
                ..mr(101)
            }),
        ),
        task(2, Some(mr(102))),
    ]);
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Char('d'));
    let painted = dialog_text(&mut state);
    assert!(painted.contains("$ ./scripts/deploy-prod.sh"), "{painted}");
    assert!(painted.contains("in /repos/aurora"), "{painted}");
    assert!(painted.contains("ships from main"), "{painted}");
    assert!(
        painted.contains("⚠ 1 unmerged MR in the Deployed stage — they will NOT ship."),
        "{painted}"
    );
    assert!(
        painted.contains("On success, 1 task with merged MRs → Complete."),
        "{painted}"
    );
    assert!(
        painted.contains("This runs the production deploy now."),
        "{painted}"
    );
}

#[test]
fn the_deploy_confirmation_starts_on_cancel_and_the_second_option_deploys() {
    let (_dir, mut state) = deploy_state(Vec::new());
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Char('d'));
    // Enter-mashing through the highest-consequence dialog in the app cancels.
    assert_eq!(handle(&mut state, KeyCode::Enter), DialogOutcome::Close);

    press(&mut state, KeyCode::Char('d'));
    handle(&mut state, KeyCode::Right);
    match handle(&mut state, KeyCode::Enter) {
        DialogOutcome::Act(Action::StartDeploy { project }) => assert_eq!(project, PROJECT),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unconfigured_project_offers_to_be_configured_instead_of_deployed() {
    let (_dir, mut state) = crate::ui::tests::temp_state();
    state.view = View::Deploy;
    state.apply_deploy(DeployUpdate::loaded(DeployBoard {
        configured: true,
        project_names: vec!["Atlas".to_string()],
        projects: [(
            "Atlas".to_string(),
            DeployProjectState {
                project_id: 4,
                ..DeployProjectState::default()
            },
        )]
        .into_iter()
        .collect(),
    }));
    select(&mut state, "dp:Atlas");
    press(&mut state, KeyCode::Char('d'));
    assert!(dialog_text(&mut state).contains("No deploy command configured"));
    handle(&mut state, KeyCode::Right);
    assert_eq!(
        handle(&mut state, KeyCode::Enter),
        DialogOutcome::Configure("Atlas".to_string())
    );
}

#[test]
fn the_conflict_confirmation_names_the_session_and_the_folder() {
    // "Resume the last session" silently starting a blank one in the wrong
    // repository is the failure mode this dialog exists to prevent.
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            conflicts: true,
            ..mr(101)
        }),
    )]);
    state.apply_board(BoardUpdate {
        archived_tasks: vec![1],
        task_sessions: [(
            1,
            TaskLink {
                cwd: "/repos/aurora".to_string(),
                session_id: "abcd1234efgh".to_string(),
                status: Some(TaskLinkStatus::Ended),
                ..TaskLink::default()
            },
        )]
        .into_iter()
        .collect(),
        ..BoardUpdate::default()
    });
    select(&mut state, "dt:1");
    press_shift(&mut state, 'R');
    let painted = dialog_text(&mut state);
    assert!(painted.contains("has conflicts"), "{painted}");
    assert!(
        painted.contains("Resumes the original session abcd1234"),
        "which session: {painted}"
    );
    assert!(
        painted.contains("in /repos/aurora"),
        "which folder: {painted}"
    );
    assert!(painted.contains("It merges development in"), "{painted}");
    assert!(painted.contains("Resume & resolve"), "{painted}");
}

#[test]
fn the_conflict_confirmation_says_when_it_would_start_fresh() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            conflicts: true,
            ..mr(101)
        }),
    )]);
    select(&mut state, "dt:1");
    press_shift(&mut state, 'R');
    let painted = dialog_text(&mut state);
    assert!(
        painted.contains("No archived transcript — starts a fresh session."),
        "{painted}"
    );
    assert!(painted.contains("Start & resolve"), "{painted}");
}

#[test]
fn confirming_a_conflict_resolution_resumes_with_the_conflict_pipeline() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            conflicts: true,
            ..mr(101)
        }),
    )]);
    state.apply_board(BoardUpdate {
        task_sessions: [(
            1,
            TaskLink {
                cwd: "/repos/aurora".to_string(),
                session_id: "abcd1234".to_string(),
                ..TaskLink::default()
            },
        )]
        .into_iter()
        .collect(),
        ..BoardUpdate::default()
    });
    state.take_actions();
    select(&mut state, "dt:1");
    press_shift(&mut state, 'R');
    handle(&mut state, KeyCode::Right);
    let outcome = handle(&mut state, KeyCode::Enter);
    crate::ui::keys::apply_outcome_for_test(&mut state, outcome);

    let queued = state.take_actions();
    let Some(Action::Resume(request)) = queued
        .into_iter()
        .find(|action| matches!(action, Action::Resume(_)))
    else {
        panic!("no resume was queued");
    };
    assert_eq!(request.purpose, crate::ui::board::ResumePurpose::Conflict);
    assert_eq!(request.link_cwd, "/repos/aurora");
    // Never moves the stage: the task is already in Deployed and it is the
    // merge that is outstanding, not the work.
    assert!(request.stage_move.is_none());

    let mr_vars = request.prompt.mr.clone().expect("the MR rides along");
    assert_eq!(mr_vars.iid, 101);
    assert_eq!(mr_vars.target_branch, "development");

    // The prompt is the `conflict` pipeline's, and it names the branch to merge.
    let spec = request
        .spec("abcd1234", "/repos/aurora", None)
        .expect("a spec");
    let prompt = spec.prompt.expect("a prompt");
    assert!(prompt.contains("!101"), "{prompt}");
    assert!(prompt.contains("development"), "{prompt}");
    assert!(spec.flags.contains("--resume abcd1234"), "{}", spec.flags);
}

#[test]
fn a_conflict_with_nothing_archived_still_starts_a_session() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            conflicts: true,
            ..mr(101)
        }),
    )]);
    // No link and no archive, but a configured repo folder for the project.
    let _ = state.config.add_odoo_project_dir(PROJECT, "/repos/aurora");
    select(&mut state, "dt:1");
    press_shift(&mut state, 'R');
    handle(&mut state, KeyCode::Right);
    let outcome = handle(&mut state, KeyCode::Enter);
    crate::ui::keys::apply_outcome_for_test(&mut state, outcome);

    let Some(Action::Resume(request)) = state
        .take_actions()
        .into_iter()
        .find(|action| matches!(action, Action::Resume(_)))
    else {
        panic!("no resume was queued");
    };
    assert_eq!(request.link_cwd, "/repos/aurora");
    assert!(request.purpose.starts_fresh(), "a fresh session is allowed");
    let spec = request.spec("", "/repos/aurora", None).expect("a spec");
    assert!(
        !spec.flags.contains("--resume"),
        "nothing to resume: {}",
        spec.flags
    );
    assert!(spec.say.contains("no prior transcript"), "{}", spec.say);
}
