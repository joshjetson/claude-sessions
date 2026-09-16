//! The Deploy tab's rows.
//!
//! The badge LEADS the row rather than trailing the task name. It is the thing
//! the tab exists to answer, and the list pane is narrow enough that anything
//! after the name gets clipped.

use std::collections::HashMap;

use chrono::{DateTime, Local};

use super::format::deadline_row;
use super::row::{Role, Row, RowBuilder, Style};
use crate::types::{DeployBoard, DeployRun, DeployRunStatus, DeployTask, MergeRequest};
use crate::util::truncate;

pub fn deploy_project_key(name: &str) -> String {
    format!("dp:{name}")
}

/// One row of the deploy tab.
#[derive(Debug, Clone, PartialEq)]
pub enum DeployItem<'a> {
    Project {
        name: &'a str,
        project_id: i64,
        missing: bool,
        command: &'a str,
        target_branch: &'a str,
        task_count: usize,
        /// Tasks whose MR has not merged — what still stands in the way.
        blocked_count: usize,
        run: Option<&'a DeployRun>,
        expanded: bool,
    },
    Task {
        task: &'a DeployTask,
        project_name: &'a str,
    },
    /// A message in place of rows: nothing configured, project not found in
    /// Odoo, or nothing left to ship.
    Info {
        name: &'a str,
        project_name: &'a str,
    },
}

pub fn deploy_item_key(item: &DeployItem<'_>) -> String {
    match item {
        DeployItem::Project { name, .. } => deploy_project_key(name),
        DeployItem::Task { task, .. } => format!("dt:{}", task.id),
        DeployItem::Info { name, project_name } => format!("di:{name}:{project_name}"),
    }
}

/// Flatten the deploy board into rows. `runs` carries deploys the dashboard
/// started and is still watching.
pub fn build_deploy_tree<'a>(
    deploy: &'a DeployBoard,
    expanded: &std::collections::HashSet<String>,
    runs: &'a HashMap<String, DeployRun>,
) -> Vec<DeployItem<'a>> {
    let mut items = Vec::new();
    if !deploy.configured {
        items.push(DeployItem::Info {
            name: "no-projects",
            project_name: "",
        });
        return items;
    }

    for name in &deploy.project_names {
        let Some(project) = deploy.projects.get(name) else {
            continue;
        };
        let is_expanded = expanded.contains(&deploy_project_key(name));
        items.push(DeployItem::Project {
            name,
            project_id: project.project_id,
            missing: project.missing,
            command: &project.command,
            target_branch: &project.target_branch,
            task_count: project.tasks.len(),
            blocked_count: project
                .tasks
                .iter()
                .filter(|task| task.mr.as_ref().map(|mr| mr.state.as_str()) != Some("merged"))
                .count(),
            run: runs.get(name),
            expanded: is_expanded,
        });
        if !is_expanded {
            continue;
        }
        if project.missing {
            items.push(DeployItem::Info {
                name: "missing-project",
                project_name: name,
            });
            continue;
        }
        if project.tasks.is_empty() {
            items.push(DeployItem::Info {
                name: "clear",
                project_name: name,
            });
            continue;
        }
        for task in &project.tasks {
            items.push(DeployItem::Task {
                task,
                project_name: name,
            });
        }
    }
    items
}

/// Glyph plus short verdict for an MR's live state.
fn mr_badge(task: &DeployTask) -> Row {
    if task.mr_url.is_empty() && task.mr_iid.is_none() {
        return badge("○ no MR      ", Role::Dim);
    }
    let Some(mr) = &task.mr else {
        if task.mr_error.is_some() {
            return badge("✗ MR error   ", Role::Danger);
        }
        let iid = task
            .mr_iid
            .map(|iid| iid.to_string())
            .unwrap_or_else(|| "?".to_string());
        return badge(format!("○ !{iid} ?"), Role::Dim);
    };

    let iid = format!("!{}", mr.iid);
    match verdict(mr) {
        Verdict::Merged => badge(format!("✓ {}", pad(&format!("{iid} merged"))), Role::Ok),
        Verdict::Closed => badge(format!("✗ {}", pad(&format!("{iid} closed"))), Role::Danger),
        Verdict::Draft => badge(format!("⚠ {}", pad(&format!("{iid} draft"))), Role::Warn),
        Verdict::Conflict => badge(
            format!("✗ {}", pad(&format!("{iid} conflict"))),
            Role::Danger,
        ),
        Verdict::Blocked(status) => {
            badge(format!("⚠ {}", pad(&format!("{iid} {status}"))), Role::Warn)
        }
        Verdict::Ready => badge(format!("⇅ {}", pad(&format!("{iid} ready"))), Role::Ready),
    }
}

enum Verdict {
    Merged,
    Closed,
    Draft,
    Conflict,
    /// GitLab says it cannot merge yet, with its own word for why.
    Blocked(String),
    Ready,
}

fn verdict(mr: &MergeRequest) -> Verdict {
    if mr.state == "merged" {
        return Verdict::Merged;
    }
    if mr.state == "closed" {
        return Verdict::Closed;
    }
    if mr.draft {
        return Verdict::Draft;
    }
    if mr.conflicts {
        return Verdict::Conflict;
    }
    if !mr.merge_status.is_empty() && mr.merge_status != "mergeable" {
        return Verdict::Blocked(truncate(&mr.merge_status.replace('_', " "), 8));
    }
    Verdict::Ready
}

/// Pipeline status dot — the other thing that decides whether a merge sticks.
fn pipe_badge(task: &DeployTask) -> Row {
    let Some(pipeline) = task
        .mr
        .as_ref()
        .map(|mr| mr.pipeline.as_str())
        .filter(|status| !status.is_empty())
    else {
        return Row::new();
    };
    let (glyph, role) = match pipeline {
        "success" => ("●", Role::Ok),
        "failed" => ("●", Role::Danger),
        "running" | "pending" => ("◐", Role::Warn),
        _ => ("○", Role::Dim),
    };
    let mut row = RowBuilder::new();
    row.plain(" ").styled(glyph, role);
    row.build()
}

pub fn format_deploy_item(item: &DeployItem<'_>, now: DateTime<Local>) -> Row {
    let mut row = RowBuilder::new();
    match item {
        DeployItem::Info { name, project_name } => match *name {
            "no-projects" => {
                row.plain("  ").styled(
                    "No projects configured for deploy. Press c to add one.",
                    Role::Dim,
                );
            }
            "missing-project" => {
                row.plain("     ").styled(
                    format!("No Odoo project named \"{project_name}\"."),
                    Role::Danger,
                );
            }
            "clear" => {
                row.plain("     ").styled(
                    "✓ Nothing outstanding in Deployed — clear to ship.",
                    Role::Ok,
                );
            }
            other => {
                row.plain("  ").styled(other, Role::Dim);
            }
        },

        DeployItem::Project {
            name,
            missing,
            command,
            target_branch,
            task_count,
            run,
            expanded,
            ..
        } => {
            row.plain(if *expanded { "▼" } else { "▶" })
                .plain(" 🚀 ")
                .push(truncate(name, 22), Style::plain().bold())
                .plain("  ");
            if *missing {
                row.styled("not found in Odoo", Role::Danger);
            } else if *task_count == 0 {
                row.styled("clear to deploy", Role::Ok);
            } else {
                row.styled(format!("{task_count} outstanding"), Role::Warn);
            }
            row.plain("  ");
            if command.is_empty() {
                row.styled("no deploy command", Role::Danger);
            } else {
                let branch = if target_branch.is_empty() {
                    "main"
                } else {
                    target_branch
                };
                row.styled(format!("→ {}", truncate(branch, 12)), Role::Dim);
            }
            if let Some(run) = run {
                match run.status {
                    DeployRunStatus::Running => {
                        row.plain("  ").styled("⟳ deploying…", Role::Warn);
                    }
                    DeployRunStatus::Ok => {
                        row.plain("  ").styled("✓ deployed", Role::Ok);
                    }
                    DeployRunStatus::Fail => {
                        row.plain("  ").styled(
                            format!(
                                "✗ deploy failed ({})",
                                run.exit_code
                                    .map(|code| code.to_string())
                                    .unwrap_or_else(|| "?".to_string())
                            ),
                            Role::Danger,
                        );
                    }
                }
            }
        }

        DeployItem::Task { task, .. } => {
            row.plain("  ")
                .extend(mr_badge(task))
                .extend(pipe_badge(task))
                .plain(" ")
                .styled(format!("#{}", task.id), Role::Id)
                .plain(" ");
            // Deploy rows keep the star's column even when unstarred, so the
            // names line up down the list.
            if task.priority.as_deref().is_some_and(|value| value != "0") {
                row.styled("★", Role::Warn);
            } else {
                row.plain(" ");
            }
            row.plain(truncate(&task.name, 34))
                .plain("  ")
                .styled(truncate(&task.state_label, 16), Role::Accent)
                .extend(deadline_row(task.deadline.as_deref(), now));
        }
    }
    row.build()
}

fn badge(text: impl Into<String>, role: Role) -> Row {
    let mut row = RowBuilder::new();
    row.styled(text, role);
    row.build()
}

/// The verdicts are padded to a common width so the id, the name and the state
/// all line up whatever the verdict says.
fn pad(text: &str) -> String {
    let width = text.chars().count();
    format!("{text}{}", " ".repeat(12usize.saturating_sub(width)))
}
