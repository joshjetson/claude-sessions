//! The Deploy tab's right-hand pane: a task, a project, or a run's output.
//!
//! Ported from `showDeployTaskDetail` / `showDeployProjectDetail` /
//! `showDeployRun` in the Node app's `src/tui/deployactions.js`. Rows rather
//! than strings, like the board's pane, so both are styled by the same
//! [`Role`] table and a test can read the text without parsing escapes.

use crate::board::{Role, Row, RowBuilder, Style};
use crate::deploy::{has_conflicts, merge_readiness};
use crate::types::{DeployRun, DeployRunStatus, DeployTask};
use crate::ui::board::BoardDetail;
use crate::ui::state::AppState;
use crate::util::truncate;

use super::slice::DeploySlice;

pub const TASK_LABEL: &str = " Deploy · Task ";
pub const PROJECT_LABEL: &str = " Deploy · Project ";
pub const OUTPUT_LABEL: &str = " Deploy · Output ";

fn line(text: impl Into<String>, role: Role) -> Row {
    let mut row = RowBuilder::new();
    row.styled(text, role);
    row.build()
}

fn bold(text: impl Into<String>) -> Row {
    let mut row = RowBuilder::new();
    row.push(text, Style::plain().bold());
    row.build()
}

fn labelled(label: &str, value: impl Into<String>) -> Row {
    let mut row = RowBuilder::new();
    row.styled(label.to_string(), Role::Dim).plain(value.into());
    row.build()
}

fn blank() -> Row {
    Row::new()
}

/// One task, with everything known about its merge request and — the point of
/// the pane — exactly why it can or cannot be merged.
/// `dev_actions` is false for the QA role, which is not offered conflict
/// resolution, so neither is the pane.
pub fn task_rows(task: &DeployTask, resumable: bool, dev_actions: bool) -> Vec<Row> {
    let mut rows = vec![
        bold(task.name.clone()),
        line(format!("#{}  ·  {}", task.id, task.project_name), Role::Dim),
        line(
            format!("{}  ·  {}", task.stage_name, task.state_label),
            Role::Accent,
        ),
    ];
    if let Some(deadline) = task.deadline.as_deref().filter(|d| !d.is_empty()) {
        rows.push(line(format!("deadline: {deadline}"), Role::Dim));
    }
    rows.push(blank());
    if !task.branch.is_empty() {
        rows.push(labelled("branch: ", task.branch.clone()));
    }

    match (&task.mr, &task.mr_error) {
        _ if task.mr_url.is_empty() && task.mr_iid.is_none() => {
            rows.push(line("No merge request linked to this task.", Role::Warn));
        }
        (_, Some(error)) => {
            rows.push(line(
                format!("Could not read MR from GitLab: {error}"),
                Role::Danger,
            ));
            let cached = if task.mr_state.is_empty() {
                "unknown"
            } else {
                &task.mr_state
            };
            rows.push(line(format!("Odoo's cached state: {cached}"), Role::Dim));
        }
        (None, None) => rows.push(line("Loading MR status from GitLab…", Role::Dim)),
        (Some(mr), None) => {
            let mut head = RowBuilder::new();
            head.styled("MR: ", Role::Dim)
                .plain(format!("!{}  {}", mr.iid, mr.title));
            rows.push(head.build());
            rows.push(line(
                format!("{} → {}", mr.source_branch, mr.target_branch),
                Role::Dim,
            ));
            let mut state = RowBuilder::new();
            state.styled("state: ", Role::Dim).plain(mr.state.clone());
            if mr.draft {
                state.plain("  ").styled("(draft)", Role::Warn);
            }
            rows.push(state.build());
            if !mr.merge_status.is_empty() {
                rows.push(labelled(
                    "merge status: ",
                    mr.merge_status.replace('_', " "),
                ));
            }
            if !mr.pipeline.is_empty() {
                rows.push(labelled("pipeline: ", mr.pipeline.clone()));
            }
            if !mr.author.is_empty() {
                rows.push(labelled("author: ", mr.author.clone()));
            }
        }
    }

    rows.push(blank());
    let readiness = merge_readiness(task);
    rows.push(if readiness.ready {
        line("✓ Ready to merge — press m", Role::Ready)
    } else {
        line(format!("Cannot merge: {}", readiness.reason), Role::Warn)
    });

    // Conflicts are the one merge blocker an agent can actually clear, so the
    // offer sits right under the verdict.
    if has_conflicts(task) && dev_actions {
        rows.push(blank());
        rows.push(line(
            if resumable {
                "🔀 Press R to resume this task's original session and resolve the conflicts."
            } else {
                "🔀 Press R to start a session that resolves the conflicts."
            },
            Role::Accent,
        ));
    }

    rows.push(blank());
    rows.push(line(
        if dev_actions {
            "m merge  ·  R resolve conflicts  ·  g session  ·  o open MR  ·  t open task  ·  d deploy"
        } else {
            "m merge  ·  g session  ·  o open MR  ·  t open task  ·  d deploy"
        },
        Role::Dim,
    ));
    rows
}

/// One project: what it would run, and what still stands in the way.
pub fn project_rows(name: &str, deploy: &DeploySlice) -> Vec<Row> {
    let project = deploy.project(name);
    let mut rows = vec![bold(format!("🚀 {name}")), blank()];

    match project.filter(|project| !project.command.is_empty()) {
        Some(project) => {
            rows.push(labelled("command: ", project.command.clone()));
            rows.push(labelled(
                "ships from: ",
                if project.target_branch.is_empty() {
                    "main".to_string()
                } else {
                    project.target_branch.clone()
                },
            ));
        }
        None => {
            rows.push(line("No deploy command configured.", Role::Danger));
            rows.push(line(
                "Press c to set one, or add it to ~/.claude-sessions.json:",
                Role::Dim,
            ));
            rows.push(line(
                format!("  deploy.projects[\"{name}\"].command"),
                Role::Dim,
            ));
        }
    }
    rows.push(blank());

    match project {
        None => rows.push(line(
            format!("No Odoo project matches \"{name}\"."),
            Role::Danger,
        )),
        Some(project) if project.missing => rows.push(line(
            format!("No Odoo project matches \"{name}\"."),
            Role::Danger,
        )),
        Some(project) if project.tasks.is_empty() => {
            rows.push(line(
                "✓ Nothing outstanding in the Deployed stage.",
                Role::Ok,
            ));
            rows.push(line(
                "Everything there is Complete or Done — clear to ship.",
                Role::Dim,
            ));
        }
        Some(project) => {
            let count = project.tasks.len();
            let plural = if count == 1 { "" } else { "s" };
            rows.push(line(
                format!("{count} task{plural} in Deployed still unfinished:"),
                Role::Warn,
            ));
            rows.push(blank());
            for task in &project.tasks {
                let readiness = merge_readiness(task);
                let mut row = RowBuilder::new();
                if readiness.ready {
                    row.styled("✓", Role::Ready);
                } else {
                    row.styled("•", Role::Warn);
                }
                row.plain(format!(" #{} {}", task.id, truncate(&task.name, 46)));
                rows.push(row.build());
                rows.push(line(
                    format!(
                        "   {} — {}",
                        task.state_label,
                        if readiness.ready {
                            "ready to merge"
                        } else {
                            &readiness.reason
                        }
                    ),
                    Role::Dim,
                ));
            }
        }
    }

    rows.push(blank());
    rows.push(line(
        "d deploy  ·  M merge all ready  ·  c configure  ·  r refresh",
        Role::Dim,
    ));
    rows
}

/// A run's captured output, headed by what is running and how it ended.
pub fn run_rows(run: &DeployRun) -> Vec<Row> {
    let mut rows = vec![
        bold(format!("🚀 {}", run.project)),
        line(format!("$ {}", run.command), Role::Dim),
    ];
    rows.push(match run.status {
        DeployRunStatus::Running => line("⟳ running…", Role::Warn),
        DeployRunStatus::Ok => line("✓ finished (exit 0)", Role::Ok),
        DeployRunStatus::Fail => line(
            format!(
                "✗ failed (exit {})",
                run.exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signalled".to_string())
            ),
            Role::Danger,
        ),
    });
    // Said explicitly: a pane that silently showed the last 200 lines of 9,000
    // would read as the whole log.
    if run.total_lines > run.lines.len() {
        rows.push(line(
            format!(
                "showing the last {} of {} lines — press L for the full log",
                run.lines.len(),
                run.total_lines
            ),
            Role::Dim,
        ));
    }
    rows.push(blank());
    rows.extend(run.lines.iter().map(|text| {
        let mut row = RowBuilder::new();
        row.plain(text.clone());
        row.build()
    }));
    rows
}

/// Put the pane on whatever the cursor is now on.
///
/// A finished-or-running deploy owns the pane: that output is what you are
/// watching for, and the project summary is only interesting between runs.
pub fn show_row(state: &mut AppState, row: &super::view::DeployRow) {
    use super::view::DeployRow;
    match row {
        DeployRow::Task { task, .. } => {
            let resumable = state.board.archived_tasks.contains(&task.id);
            let rows = task_rows(task, resumable, state.role.shows_dev_actions());
            set_detail(state, TASK_LABEL, rows, Some(task.id), None);
        }
        DeployRow::Project { name } => show_project(state, &name.clone()),
        DeployRow::Inert => {}
    }
}

pub fn show_project(state: &mut AppState, name: &str) {
    if state
        .deploy
        .run(name)
        .is_some_and(|run| !run.lines.is_empty())
    {
        return show_run(state, name);
    }
    let rows = project_rows(name, &state.deploy);
    set_detail(state, PROJECT_LABEL, rows, None, None);
}

pub fn show_run(state: &mut AppState, project: &str) {
    let Some(run) = state.deploy.run(project) else {
        return show_project(state, project);
    };
    let running = run.status == DeployRunStatus::Running;
    let rows = run_rows(run);
    set_detail(state, OUTPUT_LABEL, rows, None, Some(project.to_string()));
    // A running deploy scrolls with its output rather than sitting at the top.
    state.conv.stick = running;
}

fn set_detail(
    state: &mut AppState,
    label: &str,
    rows: Vec<Row>,
    task_id: Option<i64>,
    watching: Option<String>,
) {
    state.deploy.detail = Some(BoardDetail {
        label: label.to_string(),
        rows,
        task_id,
    });
    state.deploy.watching = watching;
    state.conv.scroll_top = 0;
    state.conv.stick = false;
    state.flash = None;
    state.dirty = true;
}

/// Repaint the output pane when the run it is showing changed.
///
/// Only that pane: an unrelated project's line must not pull the detail view
/// off whatever the cursor is on.
pub fn redraw(state: &mut AppState, project: &str) {
    state.dirty = true;
    if state.deploy.watching.as_deref() == Some(project) {
        show_run(state, project);
    }
}

/// Redraw whatever pane is open, after the state behind it changed.
pub fn refresh_detail(state: &mut AppState) {
    if let Some(project) = state.deploy.watching.clone() {
        show_run(state, &project);
        return;
    }
    let row = super::view::snapshot(state).row;
    if state.deploy.detail.is_some() {
        show_row(state, &row);
    }
}
