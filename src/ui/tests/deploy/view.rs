//! The rows, the cursor, and the three panes.

use crossterm::event::KeyCode;

use super::{deploy_state, detail_text, mr, press, select, task, PROJECT};
use crate::types::{DeployRun, DeployRunStatus, MergeRequest};
use crate::ui::board::BoardUpdate;
use crate::ui::deploy::{self, DeployUpdate};
use crate::ui::state::View;

// --- rows ------------------------------------------------------------------------

#[test]
fn a_loaded_board_opens_every_project_and_lists_its_tasks() {
    let (_dir, state) = deploy_state(vec![task(1, Some(mr(101))), task(2, None)]);
    let keys = deploy::snapshot(&state).keys;
    assert_eq!(keys, ["dp:Aurora", "dt:1", "dt:2"]);
}

#[test]
fn an_unloaded_board_says_it_only_loads_when_asked() {
    let (_dir, mut state) = crate::ui::tests::temp_state();
    state.view = View::Deploy;
    let window = deploy::window(&state, 0, 10);
    assert!(window.lines.len() == 1);
    let painted = window.lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(painted.contains("Press r to load"), "{painted}");
}

#[test]
fn an_error_is_shown_with_the_retry_it_asks_for() {
    let (_dir, mut state) = deploy_state(Vec::new());
    state.apply_deploy(DeployUpdate::failed("glab: command not found"));
    let window = deploy::window(&state, 0, 10);
    let painted: String = window
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
        .collect();
    assert!(painted.contains("glab: command not found"), "{painted}");
    assert!(painted.contains("Press r to retry"), "{painted}");
}

#[test]
fn the_label_counts_outstanding_tasks_and_running_deploys() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101))), task(2, None)]);
    assert_eq!(deploy::label(&state.deploy), " Deploy · 2 outstanding ");
    state
        .deploy
        .set_run(DeployRun::started(PROJECT, "./deploy.sh", "now"));
    assert_eq!(
        deploy::label(&state.deploy),
        " Deploy · 2 outstanding · 1 running "
    );
}

#[test]
fn collapsing_a_project_hides_its_tasks_and_a_task_row_collapses_its_project() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Left);
    assert_eq!(deploy::snapshot(&state).keys, ["dp:Aurora"]);

    press(&mut state, KeyCode::Right);
    assert_eq!(deploy::snapshot(&state).keys, ["dp:Aurora", "dt:1"]);

    // ← on a task row collapses the project it sits in, so a long list can be
    // escaped without scrolling back to the header.
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Left);
    assert_eq!(deploy::snapshot(&state).keys, ["dp:Aurora"]);
}

// --- panes ---------------------------------------------------------------------------

#[test]
fn previewing_a_task_spells_out_the_merge_verdict() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Right);
    let pane = detail_text(&state);
    assert!(pane.contains("!101"), "{pane}");
    assert!(pane.contains("task-101-work → development"), "{pane}");
    assert!(pane.contains("pipeline: success"), "{pane}");
    assert!(pane.contains("author: dev"), "{pane}");
    assert!(pane.contains("✓ Ready to merge — press m"), "{pane}");
}

#[test]
fn a_blocked_task_names_the_blocker_in_the_pane() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            draft: true,
            ..mr(101)
        }),
    )]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Right);
    assert!(detail_text(&state).contains("Cannot merge: MR is a draft"));
}

#[test]
fn a_conflicted_task_offers_the_session_it_would_resume() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            conflicts: true,
            ..mr(101)
        }),
    )]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Right);
    assert!(
        detail_text(&state).contains("Press R to start a session"),
        "with nothing archived it must not promise a resume"
    );

    state.apply_board(BoardUpdate {
        archived_tasks: vec![1],
        ..BoardUpdate::default()
    });
    press(&mut state, KeyCode::Right);
    assert!(detail_text(&state).contains("resume this task's original session"));
}

#[test]
fn a_project_pane_lists_what_stands_in_the_way() {
    let (_dir, mut state) = deploy_state(vec![
        task(1, Some(mr(101))),
        task(
            2,
            Some(MergeRequest {
                conflicts: true,
                ..mr(102)
            }),
        ),
    ]);
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Right);
    let pane = detail_text(&state);
    assert!(pane.contains("./scripts/deploy-prod.sh"), "{pane}");
    assert!(pane.contains("ships from: main"), "{pane}");
    assert!(
        pane.contains("2 tasks in Deployed still unfinished"),
        "{pane}"
    );
    assert!(pane.contains("ready to merge"), "{pane}");
    assert!(pane.contains("has merge conflicts"), "{pane}");
}

#[test]
fn an_empty_project_says_it_is_clear_to_ship() {
    let (_dir, mut state) = deploy_state(Vec::new());
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Right);
    let pane = detail_text(&state);
    assert!(pane.contains("✓ Nothing outstanding"), "{pane}");
    assert!(pane.contains("clear to ship"), "{pane}");
}

#[test]
fn the_output_pane_says_how_much_of_the_log_it_is_showing() {
    let (_dir, mut state) = deploy_state(Vec::new());
    state.deploy.set_run(DeployRun {
        lines: (0..200).map(|i| format!("line {i}")).collect(),
        total_lines: 9412,
        ..DeployRun::started(PROJECT, "./deploy.sh", "now")
    });
    deploy::show_run(&mut state, PROJECT);
    let pane = detail_text(&state);
    assert!(pane.contains("$ ./deploy.sh"), "{pane}");
    assert!(pane.contains("⟳ running…"), "{pane}");
    // Said explicitly: a pane that silently showed 200 of 9,412 would read as
    // the whole log.
    assert!(
        pane.contains("showing the last 200 of 9412 lines"),
        "{pane}"
    );
    assert!(state.conv.stick, "a running deploy follows its output");
}

#[test]
fn a_finished_run_reports_its_exit_code_and_stops_following() {
    let (_dir, mut state) = deploy_state(Vec::new());
    state.deploy.set_run(DeployRun {
        status: DeployRunStatus::Fail,
        exit_code: Some(2),
        ..DeployRun::started(PROJECT, "./deploy.sh", "now")
    });
    deploy::show_run(&mut state, PROJECT);
    assert!(detail_text(&state).contains("✗ failed (exit 2)"));
    assert!(!state.conv.stick);
}
