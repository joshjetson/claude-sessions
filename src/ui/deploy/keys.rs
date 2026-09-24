//! The Deploy tab's key map.
//!
//! Pure, like the board's: every key moves the cursor, opens a dialog, or
//! enqueues an [`Action`]. Nothing here merges, spawns or talks to GitLab — a
//! merge leaves as an action and the worker runs it, which is why the whole
//! key map can be replayed in a test with no `glab` on the machine.

use crossterm::event::{KeyCode, KeyEvent};

use crate::deploy::{has_conflicts, merge_readiness};
use crate::types::DeployTask;
use crate::ui::dialogs::{
    DeployConfig, DeployConfirm, DeployMenu, DeployTaskMenu, Dialog, MergeAllConfirm, MergeConfirm,
    ResolveConflictConfirm,
};
use crate::ui::state::{Action, AppState, MergeTarget};

use super::detail;
use super::view::{snapshot, DeployRow, DeploySnapshot};

/// Route one key on the Deploy tab.
pub fn handle_deploy(state: &mut AppState, key: KeyEvent) {
    let snapshot = snapshot(state);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.deploy_sel.move_by(&snapshot.keys, -1),
        KeyCode::Down | KeyCode::Char('j') => state.deploy_sel.move_by(&snapshot.keys, 1),
        KeyCode::Enter => on_select(state, &snapshot),
        KeyCode::Right => on_expand(state, &snapshot, true),
        KeyCode::Left => on_expand(state, &snapshot, false),
        KeyCode::Char(ch) => on_char(state, ch, &snapshot),
        _ => {}
    }
}

fn on_char(state: &mut AppState, ch: char, snapshot: &DeploySnapshot) {
    let project = snapshot.row.project_name().map(str::to_string);
    let task = snapshot.row.task().cloned();
    match ch {
        'm' => with_task(state, task, merge_one),
        'M' => with_project(state, project, merge_all),
        'R' => with_task(state, task, resolve_conflicts),
        'd' => with_project(state, project, |state, project| {
            open(
                state,
                Dialog::DeployConfirm(DeployConfirm::build(&project, state)),
            )
        }),
        'X' => with_project(state, project, cancel),
        'L' => with_project(state, project, |state, project| {
            if state.deploy.run(&project).is_none() {
                return state.flash(format!("{project} has not been deployed from here yet."));
            }
            detail::show_run(state, &project);
        }),
        // The same two questions the board asks of the same links, so the
        // "there isn't one" wording is written once.
        'g' => with_task(state, task, |state, task| {
            crate::ui::board::keys::go_to_session(state, task.id)
        }),
        'G' => with_task(state, task, |state, task| {
            crate::ui::board::keys::focus_terminal(state, task.id)
        }),
        'o' => with_task(state, task, |state, task| {
            if task.mr_url.is_empty() {
                return state.flash(format!("#{} has no merge request to open.", task.id));
            }
            state.enqueue(Action::OpenUrl(task.mr_url.clone()));
        }),
        't' => with_task(state, task, open_task),
        'c' => with_project(state, project, |state, project| {
            open(
                state,
                Dialog::DeployConfig(DeployConfig::new(&project, state)),
            )
        }),
        _ => {}
    }
}

fn with_task(
    state: &mut AppState,
    task: Option<DeployTask>,
    run: impl FnOnce(&mut AppState, DeployTask),
) {
    if let Some(task) = task {
        run(state, task);
    }
}

fn with_project(
    state: &mut AppState,
    project: Option<String>,
    run: impl FnOnce(&mut AppState, String),
) {
    match project {
        Some(project) => run(state, project),
        // The only rows with no project are the messages that stand in for the
        // tree, and saying so beats a key that silently does nothing.
        None => state.flash("Select a project or a task first."),
    }
}

fn on_select(state: &mut AppState, snapshot: &DeploySnapshot) {
    match &snapshot.row {
        DeployRow::Project { name } => open(
            state,
            Dialog::DeployMenu(DeployMenu::build(&name.clone(), state)),
        ),
        DeployRow::Task { task, .. } => {
            let menu = DeployTaskMenu::build(task, state);
            open(state, Dialog::DeployTaskMenu(menu));
        }
        DeployRow::Inert => {}
    }
}

/// `→` expands AND previews; `←` collapses. On a task row, which has nothing
/// of its own to expand, `←` collapses the project it sits in — the same shape
/// the sessions tree has, so a long list can be escaped without scrolling back
/// to the header.
fn on_expand(state: &mut AppState, snapshot: &DeploySnapshot, expand: bool) {
    if expand {
        if let Some(key) = snapshot.row.expand_key() {
            state.deploy.set_expanded(&key, true);
        }
        detail::show_row(state, &snapshot.row);
        state.dirty = true;
        return;
    }
    let key = match &snapshot.row {
        DeployRow::Project { name } => Some(crate::board::deploy_project_key(name)),
        DeployRow::Task { project, .. } => Some(crate::board::deploy_project_key(project)),
        DeployRow::Inert => None,
    };
    if let Some(key) = key {
        state.deploy.set_expanded(&key, false);
        state.dirty = true;
    }
}

// --- merging -----------------------------------------------------------------

/// One merge target, or the reason there is none.
pub fn merge_target(task: &DeployTask) -> Result<MergeTarget, String> {
    let readiness = merge_readiness(task);
    if !readiness.ready {
        return Err(readiness.reason);
    }
    let iid = task.mr.as_ref().map(|mr| mr.iid).or(task.mr_iid);
    match iid {
        Some(iid) => Ok(MergeTarget {
            task_id: task.id,
            name: task.name.clone(),
            iid,
            project_path: task.mr_project_path.clone(),
        }),
        None => Err("no merge request number".to_string()),
    }
}

fn merge_one(state: &mut AppState, task: DeployTask) {
    match merge_target(&task) {
        Ok(_) => open(state, Dialog::MergeConfirm(MergeConfirm::new(&task))),
        // The blocker, not "cannot merge": the reason is the whole point.
        Err(reason) => state.flash(format!("Cannot merge #{}: {reason}", task.id)),
    }
}

fn merge_all(state: &mut AppState, project: String) {
    open(
        state,
        Dialog::MergeAllConfirm(MergeAllConfirm::build(&project, state)),
    );
}

/// Every mergeable MR in a project, in board order, so merges are attributable
/// one at a time.
pub fn ready_targets(state: &AppState, project: &str) -> Vec<MergeTarget> {
    state
        .deploy
        .project(project)
        .map(|entry| {
            entry
                .tasks
                .iter()
                .filter_map(|task| merge_target(task).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Tasks with a merge request that is neither ready nor already merged — what
/// "merge all" is skipping, counted so the dialog can say so.
pub fn blocked_count(state: &AppState, project: &str) -> usize {
    state
        .deploy
        .project(project)
        .map(|entry| {
            entry
                .tasks
                .iter()
                .filter(|task| !task.mr_url.is_empty() || task.mr_iid.is_some())
                .filter(|task| !merge_readiness(task).ready && task.mr_state != "merged")
                .count()
        })
        .unwrap_or_default()
}

// --- the rest ------------------------------------------------------------------

fn cancel(state: &mut AppState, project: String) {
    if !state.deploy.is_running(&project) {
        return state.flash(format!("No deploy running for {project}."));
    }
    state.enqueue(Action::CancelDeploy {
        project: project.clone(),
    });
    state.flash(format!("Sent SIGTERM to the {project} deploy."));
}

fn resolve_conflicts(state: &mut AppState, task: DeployTask) {
    // Resolving a conflict is development work on the branch. The QA role
    // does not start it.
    if !state.role.shows_dev_actions() {
        return state.flash(format!(
            "Resolving conflicts starts development work, which the {} role does not offer.",
            state.role.as_str().to_uppercase()
        ));
    }
    if !has_conflicts(&task) {
        // Said rather than silently opening a dialog that would refuse: the
        // key only ever applies to a conflicted MR.
        return state.flash(format!("#{} does not report merge conflicts.", task.id));
    }
    open(
        state,
        Dialog::ResolveConflict(ResolveConflictConfirm::build(&task, state)),
    );
}

fn open_task(state: &mut AppState, task: DeployTask) {
    let url = crate::ui::board::task_url(state, task.id);
    if url.is_empty() {
        return state.flash("No Odoo URL configured — set odoo.url in ~/.claude-sessions.json.");
    }
    state.enqueue(Action::OpenUrl(url));
}

/// `r` on this tab. Manual by design: every refresh spends one GitLab API call
/// per open merge request, so nothing polls it.
pub fn refresh(state: &mut AppState) {
    state.deploy.loading = true;
    state.enqueue(Action::RefreshDeploy);
    state.dirty = true;
}

pub(crate) fn open(state: &mut AppState, dialog: Dialog) {
    state.dialog = Some(dialog);
    state.dirty = true;
}
