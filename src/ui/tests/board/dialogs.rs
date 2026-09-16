//! The board dialog smoke matrix: every one renders, and every one closes.
//!
//! The Node app dispatched dialogs on a string in a `switch` whose `default`
//! was silence, so a typo'd name simply did nothing. Here the dispatcher is an
//! enum and this is the second half of that guarantee: a dialog that exists and
//! paints nothing, or that cannot be dismissed, fails here rather than in front
//! of somebody.

use crossterm::event::KeyCode;

use crate::ui::actions::BoardData;
use crate::ui::dialogs::{
    AlreadyRunning, BlockedBy, ContextDialog, Dialog, DirPicker, FolderManager, NotifMenu,
    PipelineView, ProjectFilter, SavedDirPicker, StagePicker, TargetBranch, TaskAction, TaskMenu,
};
use crate::ui::state::Action;

use super::fixtures::*;

/// Every board dialog, in a state worth drawing.
fn every_dialog(state: &crate::ui::state::AppState) -> Vec<Dialog> {
    let task = task(5238, "Per-project default report templates");
    let request = start_request(&task, crate::ui::board::LaunchKind::Task);
    vec![
        Dialog::TaskMenu(TaskMenu::build(&task, state)),
        Dialog::Context(ContextDialog::new(&task, false)),
        Dialog::BlockedBy(BlockedBy::new(request.clone(), 1)),
        Dialog::AlreadyRunning(AlreadyRunning::new(
            request.clone(),
            vec![crate::ui::board::RacingSession {
                session_id: "abcdef123456".to_string(),
                cwd: "/repo".to_string(),
                status: crate::types::SessionStatus::Working,
                last_timestamp: None,
            }],
        )),
        Dialog::StagePicker(StagePicker::new(&task)),
        Dialog::NotifMenu(NotifMenu::new(notification("n1", Some(5238)), None)),
        Dialog::ProjectFilter(ProjectFilter::new(&state.config)),
        Dialog::DirPicker(DirPicker::for_launch(
            vec!["/dev/one".to_string(), "/dev/two".to_string()],
            "/dev/one".to_string(),
            request.clone(),
        )),
        Dialog::SavedDirPicker(SavedDirPicker::new(
            vec!["/dev/one".to_string(), "/dev/two".to_string()],
            request,
        )),
        Dialog::FolderManager(FolderManager::new(
            "Repo".to_string(),
            vec!["/dev/one".to_string()],
            vec!["/dev/two".to_string()],
        )),
        Dialog::TargetBranch(TargetBranch::new("Repo".to_string(), Some("main"))),
        Dialog::Pipeline(PipelineView::open("Repo".to_string(), None)),
    ]
}

#[test]
fn every_board_dialog_renders_something() {
    let (_dir, state) = board_state();
    for mut dialog in every_dialog(&state) {
        let name = dialog.name();
        let painted = super::render_one(&mut dialog, &state.config);
        assert!(
            painted.trim().chars().filter(|c| *c != '─').count() > 20,
            "{name} painted almost nothing:\n{painted}"
        );
    }
}

#[test]
fn every_board_dialog_is_dismissable() {
    let (_dir, mut state) = board_state();
    for dialog in every_dialog(&state) {
        let name = dialog.name();
        state.dialog = Some(dialog);
        super::press(&mut state, KeyCode::Esc);
        assert!(state.dialog.is_none(), "{name} would not close on Esc");
        state.take_actions();
    }
}

#[test]
fn every_board_dialog_swallows_q_rather_than_quitting_the_dashboard() {
    // A modal that let `q` through would close the dashboard from inside a
    // confirmation.
    let (_dir, mut state) = board_state();
    for dialog in every_dialog(&state) {
        let name = dialog.name();
        state.dialog = Some(dialog);
        super::press(&mut state, KeyCode::Char('q'));
        assert!(state.quit.is_none(), "{name} let q quit the dashboard");
        state.dialog = None;
        state.take_actions();
    }
}

#[test]
fn the_task_menu_lists_the_whole_action_set() {
    let (_dir, state) = board_state();
    let menu = TaskMenu::build(&task(5238, "x"), &state);
    let labels: Vec<&str> = menu
        .entries
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    for needle in [
        "Start task",
        "Add context & start",
        "QA",
        "dry run",
        "Pre-work brief",
        "Move to stage",
        "Open in browser",
        "Repo folders",
        "MR target branch",
        "Cancel",
    ] {
        assert!(
            labels.iter().any(|label| label.contains(needle)),
            "no menu entry for {needle}: {labels:?}"
        );
    }
    // The resume entries only appear once there is something to resume.
    assert!(!labels.iter().any(|label| label.contains("Resume")));
}

#[test]
fn the_resume_entries_appear_once_a_transcript_is_archived() {
    let (_dir, mut state) = board_state();
    state.board.archived_tasks.insert(5238);
    let menu = TaskMenu::build(&task(5238, "x"), &state);
    let labels: Vec<&str> = menu
        .entries
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    assert!(labels
        .iter()
        .any(|label| label.contains("Resume conversation (no prompt)")));
    assert!(labels
        .iter()
        .any(|label| label.contains("Resume for revision (prior context)")));
    assert!(labels
        .iter()
        .any(|label| label.contains("Resume for revision + add context")));
}

#[test]
fn the_live_session_entries_appear_only_while_one_is_running() {
    let (_dir, mut state) = board_state();
    assert!(!TaskMenu::build(&task(5238, "x"), &state)
        .entries
        .iter()
        .any(|(_, action)| *action == TaskAction::GoToSession));
    with_sessions(&mut state, vec![live_session("live", Some(5238), 1000)]);
    let menu = TaskMenu::build(&task(5238, "x"), &state);
    assert_eq!(menu.action(0), Some(&TaskAction::GoToSession));
    assert_eq!(menu.action(1), Some(&TaskAction::FocusTerminal));
}

#[test]
fn the_qa_label_says_what_is_missing_rather_than_implying_a_round() {
    let (_dir, state) = board_state();
    let menu = TaskMenu::build(&task(5238, "x"), &state);
    let qa = menu
        .entries
        .iter()
        .find(|(label, _)| label.contains("QA") && !label.contains("dry run"))
        .expect("a QA entry");
    assert!(qa.0.contains("extras phase"), "{}", qa.0);
}

#[test]
fn the_stage_picker_marks_the_stage_the_task_is_in() {
    let (_dir, state) = board_state();
    let mut picker = StagePicker::new(&task(5238, "x"));
    assert!(picker.accept(&BoardData::Stages {
        task_id: 5238,
        stages: vec![
            crate::odoo::StageRecord {
                id: 1,
                name: "Approved to Start".into(),
                sequence: 1
            },
            crate::odoo::StageRecord {
                id: 2,
                name: "In Progress".into(),
                sequence: 2
            },
        ],
    }));
    let mut dialog = Dialog::StagePicker(picker);
    let painted = super::render_one(&mut dialog, &state.config);
    assert!(painted.contains("(current)"), "{painted}");
    assert!(painted.contains("In Progress"), "{painted}");
}

#[test]
fn the_stage_picker_moves_only_to_a_different_stage() {
    let (_dir, mut state) = board_state();
    let mut picker = StagePicker::new(&task(5238, "x"));
    picker.accept(&BoardData::Stages {
        task_id: 5238,
        stages: vec![
            crate::odoo::StageRecord {
                id: 1,
                name: "Approved to Start".into(),
                sequence: 1,
            },
            crate::odoo::StageRecord {
                id: 2,
                name: "In Progress".into(),
                sequence: 2,
            },
        ],
    });
    state.dialog = Some(Dialog::StagePicker(picker));
    // It opens on the current stage; Enter there is a no-op close.
    super::press(&mut state, KeyCode::Enter);
    assert!(state.take_actions().is_empty());

    let mut picker = StagePicker::new(&task(5238, "x"));
    picker.accept(&BoardData::Stages {
        task_id: 5238,
        stages: vec![
            crate::odoo::StageRecord {
                id: 1,
                name: "Approved to Start".into(),
                sequence: 1,
            },
            crate::odoo::StageRecord {
                id: 2,
                name: "In Progress".into(),
                sequence: 2,
            },
        ],
    });
    state.dialog = Some(Dialog::StagePicker(picker));
    super::press(&mut state, KeyCode::Down);
    super::press(&mut state, KeyCode::Enter);
    let queued = state.take_actions();
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::MoveStage { task_id, stage_id, stage_name }
                if *task_id == 5238 && *stage_id == 2 && stage_name == "In Progress"
        )),
        "{queued:?}"
    );
}

#[test]
fn an_answer_for_another_task_is_not_accepted() {
    // A lookup that lands after the cursor moved on must not be shown against
    // the wrong row.
    let mut picker = StagePicker::new(&task(5238, "x"));
    assert!(!picker.accept(&BoardData::Stages {
        task_id: 9999,
        stages: Vec::new(),
    }));
    let mut blocked = BlockedBy::new(
        start_request(&blocked_task(5238), crate::ui::board::LaunchKind::Task),
        1,
    );
    assert!(!blocked.accept(&BoardData::Blockers {
        task_id: 9999,
        blockers: Vec::new(),
    }));
}

#[test]
fn the_project_filter_cycles_default_include_ignore_and_saves_on_enter() {
    let (_dir, mut state) = board_state();
    let mut filter = ProjectFilter::new(&state.config);
    filter.accept(&BoardData::Projects(vec![
        crate::odoo::OdooProject {
            id: 1,
            name: "Alpha".into(),
        },
        crate::odoo::OdooProject {
            id: 2,
            name: "Beta".into(),
        },
    ]));
    state.dialog = Some(Dialog::ProjectFilter(filter));

    super::press(&mut state, KeyCode::Char(' ')); // Alpha -> include
    super::press(&mut state, KeyCode::Down);
    super::press(&mut state, KeyCode::Char(' ')); // Beta -> include
    super::press(&mut state, KeyCode::Char(' ')); // Beta -> ignore
    super::press(&mut state, KeyCode::Enter);

    let saved = state.config.board_project_filter();
    assert_eq!(saved.include, vec!["Alpha".to_string()]);
    assert_eq!(saved.ignore, vec!["Beta".to_string()]);
    // The board on screen answered the old filter, so it is dropped.
    assert!(state.board.board.is_none());
    assert!(state
        .take_actions()
        .iter()
        .any(|a| matches!(a, Action::RefreshBoard(_))));
}
