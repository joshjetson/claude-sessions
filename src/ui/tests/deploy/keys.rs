//! The key map. Every side effect is asserted as a queued action.

use crossterm::event::KeyCode;

use super::{deploy_state, dialog_name, mr, press, press_shift, select, task, PROJECT};
use crate::types::{DeployRun, MergeRequest};
use crate::ui::state::{Action, AppState, View};

// --- keys ---------------------------------------------------------------------------

#[test]
fn r_is_the_only_thing_that_ever_loads_the_board() {
    // Manual by design: every refresh costs a GitLab call per open MR, so no
    // timer and no view switch may trigger one.
    let (_dir, mut state) = crate::ui::tests::temp_state();
    state.view = View::Sessions;
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.view, View::Deploy);
    assert!(
        !state
            .take_actions()
            .iter()
            .any(|action| matches!(action, Action::RefreshDeploy)),
        "switching to the tab fetched the board"
    );

    press(&mut state, KeyCode::Char('r'));
    assert!(state
        .take_actions()
        .iter()
        .any(|action| matches!(action, Action::RefreshDeploy)));
}

#[test]
fn m_on_a_ready_task_confirms_before_it_merges() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('m'));
    assert_eq!(dialog_name(&state), Some("mergeConfirm"));
    assert!(state.take_actions().is_empty(), "it merged without asking");
}

#[test]
fn m_on_a_blocked_task_names_the_blocker_and_opens_nothing() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            merge_status: "ci_still_running".to_string(),
            ..mr(101)
        }),
    )]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('m'));
    assert!(state.dialog.is_none());
    let flash = state.flash.clone().unwrap_or_default();
    assert!(
        flash.contains("Cannot merge #1: GitLab says: ci still running"),
        "{flash}"
    );
}

#[test]
fn every_deploy_key_opens_what_it_should() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    for (key, row, expected) in [
        ('M', "dp:Aurora", "mergeAllConfirm"),
        ('d', "dp:Aurora", "deployConfirm"),
        ('c', "dp:Aurora", "deployConfig"),
        ('m', "dt:1", "mergeConfirm"),
    ] {
        select(&mut state, row);
        press_shift_or_plain(&mut state, key);
        assert_eq!(dialog_name(&state), Some(expected), "key {key}");
        state.dialog = None;
    }
    // Enter opens the menu appropriate to the row.
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Enter);
    assert_eq!(dialog_name(&state), Some("deployMenu"));
    state.dialog = None;
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Enter);
    assert_eq!(dialog_name(&state), Some("deployTaskMenu"));
}

fn press_shift_or_plain(state: &mut AppState, ch: char) {
    if ch.is_uppercase() {
        press_shift(state, ch);
    } else {
        press(state, KeyCode::Char(ch));
    }
}

#[test]
fn o_and_t_open_the_merge_request_and_the_task() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101))), task(2, None)]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('o'));
    assert!(state.take_actions().iter().any(|action| matches!(
        action,
        Action::OpenUrl(url) if url.contains("merge_requests/101")
    )));

    // A task with no MR says so rather than opening nothing.
    select(&mut state, "dt:2");
    press(&mut state, KeyCode::Char('o'));
    assert!(state
        .flash
        .clone()
        .unwrap_or_default()
        .contains("no merge request to open"));

    // No Odoo URL configured, so `t` explains rather than opening a blank tab.
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Char('t'));
    assert!(state
        .flash
        .clone()
        .unwrap_or_default()
        .contains("No Odoo URL configured"));
}

#[test]
fn big_x_cancels_only_a_running_deploy() {
    let (_dir, mut state) = deploy_state(Vec::new());
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'X');
    assert!(state
        .flash
        .clone()
        .unwrap_or_default()
        .contains("No deploy running"));
    assert!(state.take_actions().is_empty());

    state
        .deploy
        .set_run(DeployRun::started(PROJECT, "./deploy.sh", "now"));
    press_shift(&mut state, 'X');
    assert!(state.take_actions().iter().any(|action| matches!(
        action,
        Action::CancelDeploy { project } if project == PROJECT
    )));
}

#[test]
fn big_l_opens_the_output_pane_only_once_there_is_one() {
    let (_dir, mut state) = deploy_state(Vec::new());
    select(&mut state, "dp:Aurora");
    press_shift(&mut state, 'L');
    assert!(state
        .flash
        .clone()
        .unwrap_or_default()
        .contains("has not been deployed from here yet"));

    state
        .deploy
        .set_run(DeployRun::started(PROJECT, "./deploy.sh", "now"));
    press_shift(&mut state, 'L');
    assert_eq!(state.deploy.watching.as_deref(), Some(PROJECT));
}

#[test]
fn big_r_only_applies_to_a_conflicted_merge_request() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dt:1");
    press_shift(&mut state, 'R');
    assert!(state.dialog.is_none());
    assert!(state
        .flash
        .clone()
        .unwrap_or_default()
        .contains("does not report merge conflicts"));
}
