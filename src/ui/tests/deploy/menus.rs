//! The two menus and the config editor, plus the smoke matrix over all eight
//! dialogs and the open-merge-request browser.

use crossterm::event::{KeyCode, KeyEvent};

use super::{deploy_state, dialog_text, handle, mr, press, select, task, BODY, PROJECT};
use crate::types::{DeployRun, MergeRequest};
use crate::ui::dialogs::{Dialog, DialogCtx, DialogOutcome};
use crate::ui::state::{Action, AppState};

#[test]
fn the_task_menu_carries_the_blocker_reason_in_the_merge_label() {
    let (_dir, mut state) = deploy_state(vec![task(
        1,
        Some(MergeRequest {
            merge_status: "not_approved".to_string(),
            ..mr(101)
        }),
    )]);
    select(&mut state, "dt:1");
    press(&mut state, KeyCode::Enter);
    let painted = dialog_text(&mut state);
    assert!(
        painted.contains("Merge — blocked: GitLab says: not approved"),
        "{painted}"
    );
}

#[test]
fn the_project_menu_changes_with_what_is_running() {
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Enter);
    let idle = dialog_text(&mut state);
    assert!(idle.contains("Merge all ready MRs (1)"), "{idle}");
    assert!(idle.contains("Deploy to production"), "{idle}");
    state.dialog = None;

    state
        .deploy
        .set_run(DeployRun::started(PROJECT, "./deploy.sh", "now"));
    press(&mut state, KeyCode::Enter);
    let running = dialog_text(&mut state);
    assert!(running.contains("View deploy output"), "{running}");
    assert!(running.contains("Cancel running deploy"), "{running}");
    assert!(
        !running.contains("Deploy to production"),
        "a second deploy must not be offered: {running}"
    );
}

#[test]
fn the_config_editor_writes_the_field_and_refetches_the_board() {
    let (_dir, mut state) = deploy_state(Vec::new());
    select(&mut state, "dp:Aurora");
    press(&mut state, KeyCode::Char('c'));
    assert!(dialog_text(&mut state).contains("./scripts/deploy-prod.sh"));

    // Enter on the command row opens the prompt; typing and submitting saves.
    handle(&mut state, KeyCode::Enter);
    for ch in "X".chars() {
        handle(&mut state, KeyCode::Char(ch));
    }
    let outcome = handle(&mut state, KeyCode::Enter);
    assert!(
        matches!(
            outcome,
            DialogOutcome::Keep {
                action: Some(Action::RefreshDeploy),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        state
            .config
            .deploy_project_config(PROJECT)
            .map(|c| c.command),
        Some("./scripts/deploy-prod.shX".to_string())
    );
}

// --- the smoke matrix -------------------------------------------------------------------

/// Every Deploy-tab dialog, in the state it opens in.
pub(crate) fn every_deploy_dialog(state: &AppState) -> Vec<Dialog> {
    let ready = task(1, Some(mr(101)));
    let conflicted = task(
        2,
        Some(MergeRequest {
            conflicts: true,
            ..mr(102)
        }),
    );
    vec![
        Dialog::DeployMenu(crate::ui::dialogs::DeployMenu::build(PROJECT, state)),
        Dialog::DeployTaskMenu(crate::ui::dialogs::DeployTaskMenu::build(&ready, state)),
        Dialog::MergeConfirm(crate::ui::dialogs::MergeConfirm::new(&ready)),
        Dialog::MergeAllConfirm(crate::ui::dialogs::MergeAllConfirm::build(PROJECT, state)),
        Dialog::DeployConfirm(crate::ui::dialogs::DeployConfirm::build(PROJECT, state)),
        Dialog::DeployConfig(crate::ui::dialogs::DeployConfig::new(PROJECT, state)),
        Dialog::ResolveConflict(crate::ui::dialogs::ResolveConflictConfirm::build(
            &conflicted,
            state,
        )),
        Dialog::OpenMrs(crate::ui::dialogs::OpenMrs::loading()),
    ]
}

#[test]
fn the_smoke_matrix_renders_every_deploy_dialog() {
    // The cheapest high-value test there is: a dialog that only fails at RENDER
    // time passes every check that merely constructs it.
    let (_dir, mut state) = deploy_state(vec![task(1, Some(mr(101)))]);
    for mut dialog in every_deploy_dialog(&state) {
        let name = dialog.name();
        state.dialog = Some(dialog.clone());
        let painted = dialog_text(&mut state);
        assert!(
            painted.chars().any(|c| c != ' '),
            "{name} rendered a blank frame"
        );
        // And it survives keys it does not know.
        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('z'),
            KeyCode::Backspace,
            KeyCode::PageDown,
            KeyCode::Tab,
        ] {
            let mut ctx = DialogCtx {
                config: &mut state.config,
            };
            dialog.handle_key(KeyEvent::from(code), BODY, &mut ctx);
        }
    }
}

#[test]
fn the_open_mr_browser_stays_open_so_several_can_be_opened() {
    let mut dialog = crate::ui::dialogs::OpenMrs::loading();
    assert!(dialog.accept(&crate::ui::actions::BoardData::OpenMrs(vec![
        crate::gitlab::OpenMr {
            iid: 403,
            title: "Widget rollout".to_string(),
            url: "https://git.example.com/group/repo/-/merge_requests/403".to_string(),
            project: "group/repo".to_string(),
            target_branch: "development".to_string(),
            draft: true,
            updated_at: String::new(),
        },
    ])));

    let (_dir, mut state) = deploy_state(Vec::new());
    state.dialog = Some(Dialog::OpenMrs(dialog));
    let painted = dialog_text(&mut state);
    assert!(painted.contains("!403"), "{painted}");
    assert!(painted.contains("[draft]"), "{painted}");
    assert!(painted.contains("repo"), "{painted}");

    match handle(&mut state, KeyCode::Enter) {
        DialogOutcome::Keep {
            action: Some(Action::OpenUrl(url)),
            ..
        } => assert!(url.contains("merge_requests/403")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_open_mr_browser_reports_a_failure_instead_of_spinning() {
    let mut dialog = crate::ui::dialogs::OpenMrs::loading();
    assert!(dialog.accept(&crate::ui::actions::BoardData::Failed {
        task_id: None,
        error: "No GitLab host configured".to_string(),
    }));
    let (_dir, mut state) = deploy_state(Vec::new());
    state.dialog = Some(Dialog::OpenMrs(dialog));
    assert!(dialog_text(&mut state).contains("No GitLab host configured"));
}
