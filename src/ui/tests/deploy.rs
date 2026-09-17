//! The Deploy tab.
//!
//! - [`view`] — the rows, the cursor, and the panes.
//! - [`keys`] — the key map.
//! - [`dialogs`] — the four irreversible confirmations.
//! - [`menus`] — the two menus, the config editor, the open-MR browser, and
//!   the smoke matrix over all eight.
//! - [`worker`] — merging and loading, off the draw thread.
//!
//! Nothing here can start a process. The key handlers are pure and every side
//! effect leaves as an [`Action`](crate::ui::state::Action), so a whole
//! session's worth of keys — including "deploy to production" — is replayed
//! against an action queue.

mod dialogs;
mod keys;
mod menus;
mod view;
mod worker;

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tempfile::TempDir;

use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::Paths;
use crate::types::{DeployBoard, DeployProjectState, DeployTask, MergeRequest};
use crate::ui::deploy::{self, DeployUpdate};
use crate::ui::dialogs::{Dialog, DialogCtx, DialogOutcome};
use crate::ui::state::{AppState, View};
use crate::ui::tests::{render, text};

pub(crate) const BODY: Rect = Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 30,
};
pub(crate) const PROJECT: &str = "Aurora";

// --- fixtures -----------------------------------------------------------------

pub(crate) fn mr(iid: i64) -> MergeRequest {
    MergeRequest {
        iid,
        url: format!("https://git.example.com/group/repo/-/merge_requests/{iid}"),
        title: format!("Merge request {iid}"),
        state: "opened".to_string(),
        source_branch: format!("task-{iid}-work"),
        target_branch: "development".to_string(),
        merge_status: "mergeable".to_string(),
        pipeline: "success".to_string(),
        author: "dev".to_string(),
        ..MergeRequest::default()
    }
}

pub(crate) fn task(id: i64, mr: Option<MergeRequest>) -> DeployTask {
    DeployTask {
        id,
        name: format!("Task {id}"),
        state: "01_in_progress".to_string(),
        state_label: "In Progress".to_string(),
        project_name: PROJECT.to_string(),
        project_id: 3,
        stage_name: "Deployed".to_string(),
        branch: format!("task-{id}-work"),
        mr_url: mr.as_ref().map(|mr| mr.url.clone()).unwrap_or_default(),
        mr_state: mr.as_ref().map(|mr| mr.state.clone()).unwrap_or_default(),
        mr_iid: mr.as_ref().map(|mr| mr.iid),
        mr_project_path: mr
            .as_ref()
            .map(|_| "group/repo".to_string())
            .unwrap_or_default(),
        mr,
        ..DeployTask::default()
    }
}

pub(crate) fn board(tasks: Vec<DeployTask>) -> DeployBoard {
    let mut projects = BTreeMap::new();
    projects.insert(
        PROJECT.to_string(),
        DeployProjectState {
            project_id: 3,
            command: "./scripts/deploy-prod.sh".to_string(),
            target_branch: "main".to_string(),
            tasks,
            ..DeployProjectState::default()
        },
    );
    DeployBoard {
        configured: true,
        project_names: vec![PROJECT.to_string()],
        projects,
    }
}

/// A dashboard on the Deploy tab with a loaded board.
pub(crate) fn deploy_state(tasks: Vec<DeployTask>) -> (TempDir, AppState) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    std::fs::write(
        &paths.config_path,
        serde_json::json!({
            "deploy": { "projects": { PROJECT: { "command": "./scripts/deploy-prod.sh",
                                                 "cwd": "/repos/aurora",
                                                 "targetBranch": "main" } } },
        })
        .to_string(),
    )
    .expect("config");
    let config = ConfigHandle::load(&paths, EnvOverrides::default());
    let mut state = AppState::new(paths, config);
    state.view = View::Deploy;
    state.apply_deploy(DeployUpdate::loaded(board(tasks)));
    state.take_actions();
    (dir, state)
}

pub(crate) fn press(state: &mut AppState, code: KeyCode) {
    crate::ui::keys::handle_key(state, KeyEvent::from(code), BODY);
}

pub(crate) fn press_shift(state: &mut AppState, ch: char) {
    crate::ui::keys::handle_key(
        state,
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT),
        BODY,
    );
}

/// Move the cursor onto the row whose key is `key`.
pub(crate) fn select(state: &mut AppState, key: &str) {
    let snapshot = deploy::snapshot(state);
    let index = snapshot
        .keys
        .iter()
        .position(|candidate| candidate == key)
        .unwrap_or_else(|| panic!("no row {key} in {:?}", snapshot.keys));
    state.deploy_sel.set(&snapshot.keys, index);
}

pub(crate) fn detail_text(state: &AppState) -> String {
    state
        .deploy
        .detail
        .as_ref()
        .map(|detail| {
            detail
                .rows
                .iter()
                .map(|row| crate::board::plain_text(row))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub(crate) fn dialog_text(state: &mut AppState) -> String {
    let mut dialog = state.dialog.take().expect("a dialog");
    let buffer = render(BODY.width, BODY.height, |frame| {
        let area = frame.area();
        dialog.render(frame, area, &state.config);
    });
    state.dialog = Some(dialog);
    text(&buffer)
}

pub(crate) fn dialog_name(state: &AppState) -> Option<&'static str> {
    state.dialog.as_ref().map(Dialog::name)
}

pub(crate) fn handle(state: &mut AppState, code: KeyCode) -> DialogOutcome {
    let mut dialog = state.dialog.take().expect("a dialog");
    let outcome = {
        let mut ctx = DialogCtx {
            config: &mut state.config,
        };
        dialog.handle_key(KeyEvent::from(code), BODY, &mut ctx)
    };
    state.dialog = Some(dialog);
    outcome
}
