//! The folder pickers, the branch override and the pipeline viewer.
//!
//! Split from the dialog smoke matrix: these are the ones that write to config
//! or to a repository, so they each need a throwaway tree.

use crossterm::event::KeyCode;

use crate::ui::dialogs::{
    ContextDialog, Dialog, DirPicker, FolderManager, NotifMenu, PipelineView, SavedDirPicker,
    TargetBranch,
};
use crate::ui::state::Action;

use super::fixtures::*;

#[test]
fn an_empty_target_branch_clears_the_override() {
    let (_dir, mut state) = board_state();
    state.config.set_target_branch("Repo", "main").unwrap();
    assert_eq!(state.config.target_branch("Repo"), Some("main"));
    state.dialog = Some(Dialog::TargetBranch(TargetBranch::new(
        "Repo".to_string(),
        Some("main"),
    )));
    for _ in 0..4 {
        super::press(&mut state, KeyCode::Backspace);
    }
    super::press(&mut state, KeyCode::Enter);
    assert_eq!(state.config.target_branch("Repo"), None);
}

#[test]
fn the_folder_manager_adds_and_removes_without_launching() {
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("Repo", "/dev/one")
        .unwrap();
    state.dialog = Some(Dialog::FolderManager(FolderManager::new(
        "Repo".to_string(),
        vec!["/dev/one".to_string()],
        vec!["/dev/two".to_string()],
    )));
    super::press(&mut state, KeyCode::Char('d'));
    assert!(state.config.odoo_project_dir_list("Repo").is_empty());
    assert!(
        state.take_actions().is_empty(),
        "managing folders launched something"
    );
}

#[test]
fn the_saved_picker_hands_the_chosen_folder_back_to_the_launch() {
    let (_dir, mut state) = board_state();
    let request = start_request(&task(5238, "x"), crate::ui::board::LaunchKind::Task);
    state.dialog = Some(Dialog::SavedDirPicker(SavedDirPicker::new(
        vec!["/dev/one".to_string(), "/dev/two".to_string()],
        request,
    )));
    super::press(&mut state, KeyCode::Down);
    super::press(&mut state, KeyCode::Enter);
    let queued = state.take_actions();
    let Some(Action::Launch(spec)) = queued.into_iter().find(|a| matches!(a, Action::Launch(_)))
    else {
        panic!("the picker did not continue the launch");
    };
    assert_eq!(spec.cwd, "/dev/two");
}

#[test]
fn picking_a_new_folder_remembers_it() {
    let (_dir, mut state) = board_state();
    let request = start_request(&task(5238, "x"), crate::ui::board::LaunchKind::Task);
    state.dialog = Some(Dialog::DirPicker(DirPicker::for_launch(
        vec!["/dev/one".to_string()],
        String::new(),
        request,
    )));
    super::press(&mut state, KeyCode::Enter);
    assert_eq!(
        state.config.odoo_project_dir_list("NoSuchProject-ForTests"),
        ["/dev/one".to_string()]
    );
}

#[test]
fn the_notification_menu_offers_go_to_session_only_when_one_resolved() {
    let (_dir, state) = board_state();
    let mut without = Dialog::NotifMenu(NotifMenu::new(notification("n", None), None));
    let painted = super::render_one(&mut without, &state.config);
    assert!(!painted.contains("Go to session"), "{painted}");

    let mut with = Dialog::NotifMenu(NotifMenu::new(
        notification("n", None),
        Some(("sess".to_string(), None, "/repo".to_string())),
    ));
    let painted = super::render_one(&mut with, &state.config);
    assert!(painted.contains("Go to session"), "{painted}");
}

#[test]
fn the_pipeline_viewer_draws_the_flow_with_its_step_count_pinned() {
    let (_dir, state) = board_state();
    let mut dialog = Dialog::Pipeline(PipelineView::open("Repo".to_string(), None));
    let painted = super::render_one(&mut dialog, &state.config);
    assert!(painted.contains("steps run"), "{painted}");
    assert!(painted.contains("default pipeline"), "{painted}");
    assert!(painted.contains("e edit in $EDITOR"), "{painted}");
}

#[test]
fn the_pipeline_viewer_says_so_when_the_project_has_no_repo() {
    let (_dir, mut state) = board_state();
    state.dialog = Some(Dialog::Pipeline(PipelineView::open(
        "Repo".to_string(),
        None,
    )));
    super::press(&mut state, KeyCode::Char('t'));
    let flash = state.flash.clone().unwrap_or_default();
    assert!(flash.contains("No repo mapped for Repo"), "{flash}");
}

#[test]
fn the_pipeline_viewer_writes_a_template_and_opens_an_editor_when_it_has_one() {
    let (_dir, mut state) = board_state();
    let repo = tempfile::tempdir().expect("repo");
    let repo_path = repo.path().to_string_lossy().into_owned();
    state.dialog = Some(Dialog::Pipeline(PipelineView::open(
        "Repo".to_string(),
        Some(repo_path.clone()),
    )));
    super::press(&mut state, KeyCode::Char('t'));
    let queued = state.take_actions();
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::WritePipelineTemplate { repo, .. } if repo == &repo_path
        )),
        "{queued:?}"
    );

    super::press(&mut state, KeyCode::Char('e'));
    let queued = state.take_actions();
    let Some(Action::OpenEditor { path, line }) = queued
        .into_iter()
        .find(|a| matches!(a, Action::OpenEditor { .. }))
    else {
        panic!("e did not open an editor");
    };
    assert!(path.ends_with(".claude-sessions/pipeline.json"), "{path}");
    // The template was written first, so `e` lands in a file that exists…
    assert!(std::path::Path::new(&path).exists(), "{path}");
    // …on the step that was selected, not the top.
    assert!(line >= 1);
}

#[test]
fn a_context_dialog_starts_with_what_was_typed() {
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    state.dialog = Some(Dialog::Context(ContextDialog::new(&task(5238, "x"), false)));
    for ch in "use the v2 endpoint".chars() {
        super::press(&mut state, KeyCode::Char(ch));
    }
    super::press_ctrl(&mut state, 's');
    let queued = state.take_actions();
    let Some(Action::Launch(spec)) = queued.into_iter().find(|a| matches!(a, Action::Launch(_)))
    else {
        panic!("the context dialog did not start anything");
    };
    assert!(
        spec.prompt
            .as_deref()
            .unwrap()
            .contains("use the v2 endpoint"),
        "{:?}",
        spec.prompt
    );
}
