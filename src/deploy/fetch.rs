//! Loading the Deploy tab: one Odoo query, then a live GitLab read per task
//! that has a merge request.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::config::ConfigHandle;
use crate::gitlab::{parse_mr_url, Gitlab};
use crate::odoo::{records, OdooClient};
use crate::types::{DeployBoard, DeployProjectState, DeployTask};

use super::{state_label, DEPLOY_STAGE, FINISHED_STATES};

/// What the deploy query asks for. The board's own `TASK_FIELDS` plus the state
/// and GitLab columns — the Deploy tab is the only view that needs either.
pub const DEPLOY_TASK_FIELDS: [&str; 12] = [
    "id",
    "name",
    "state",
    "project_id",
    "stage_id",
    "priority",
    "date_deadline",
    "gitlab_branch_name",
    "gitlab_merge_request_url",
    "gitlab_merge_request_state",
    "gitlab_merge_request_iid",
    // Not rendered, but it is what the query orders by, and Odoo is happier
    // returning a column it was asked to sort on. Node asked for it too.
    "write_date",
];

/// One configured project, as the tab needs it: the name to match in Odoo and
/// the two settings the rows display.
///
/// A resolved value rather than a `ConfigHandle` borrow so [`fetch_deploy_board`]
/// can be driven from a test with no config file, and so the worker thread
/// holds no second copy of the config to go stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployProjectSpec {
    pub name: String,
    pub command: String,
    pub target_branch: String,
}

/// The configured deploy projects, in the order the config lists them.
pub fn deploy_specs(config: &ConfigHandle) -> Vec<DeployProjectSpec> {
    let names: Vec<String> = config.deploy_project_names().map(str::to_string).collect();
    names
        .into_iter()
        .map(|name| {
            let settings = config.deploy_project_config(&name);
            DeployProjectSpec {
                command: settings
                    .as_ref()
                    .map(|c| c.command.clone())
                    .unwrap_or_default(),
                target_branch: settings
                    .as_ref()
                    .map(|c| c.target_branch.clone())
                    .unwrap_or_default(),
                name,
            }
        })
        .collect()
}

/// Load the deploy board.
///
/// A configured project with nothing outstanding still appears, with zero
/// tasks: that IS the "clear to deploy" signal, so hiding it would hide the
/// answer the tab exists to give.
pub fn fetch_deploy_board(
    odoo: &OdooClient,
    specs: &[DeployProjectSpec],
) -> Result<DeployBoard, String> {
    if specs.is_empty() {
        return Ok(DeployBoard::default());
    }

    let all = odoo.get_projects().map_err(|err| err.to_string())?;
    let mut projects: BTreeMap<String, DeployProjectState> = BTreeMap::new();
    // Odoo project id -> the configured name it answers for.
    let mut wanted: BTreeMap<i64, String> = BTreeMap::new();

    for spec in specs {
        let found = all
            .iter()
            .find(|project| project.name.eq_ignore_ascii_case(&spec.name));
        if let Some(project) = found {
            wanted.insert(project.id, spec.name.clone());
        }
        projects.insert(
            spec.name.clone(),
            DeployProjectState {
                project_id: found.map(|project| project.id).unwrap_or_default(),
                // Configured here but no such Odoo project: worth saying,
                // rather than showing an empty column.
                missing: found.is_none(),
                command: spec.command.clone(),
                target_branch: spec.target_branch.clone(),
                tasks: Vec::new(),
            },
        );
    }

    if !wanted.is_empty() {
        let ids: Vec<i64> = wanted.keys().copied().collect();
        let rows = odoo
            .execute_kw(
                "project.task",
                "search_read",
                vec![json!([
                    ["project_id", "in", ids],
                    ["stage_id.name", "=", DEPLOY_STAGE],
                    ["state", "not in", FINISHED_STATES],
                ])],
                json!({ "fields": DEPLOY_TASK_FIELDS, "order": "write_date desc" }),
            )
            .map_err(|err| err.to_string())?;
        for record in rows.as_array().into_iter().flatten() {
            let task = to_deploy_task(record);
            let Some(name) = wanted.get(&task.project_id) else {
                continue;
            };
            if let Some(project) = projects.get_mut(name) {
                project.tasks.push(task);
            }
        }
    }

    let mut board = DeployBoard {
        configured: true,
        project_names: specs.iter().map(|spec| spec.name.clone()).collect(),
        projects,
    };
    fill_missing_mr_paths(odoo, &mut board);
    Ok(board)
}

/// One `project.task` record as a deploy row.
pub fn to_deploy_task(record: &Value) -> DeployTask {
    let mr_url = records::string_or_empty(record.get("gitlab_merge_request_url"));
    let parsed = parse_mr_url(&mr_url);
    let project = records::many2one(record.get("project_id"));
    let stage = records::many2one(record.get("stage_id"));
    let state = records::string_or_empty(record.get("state"));
    DeployTask {
        id: record.get("id").and_then(Value::as_i64).unwrap_or_default(),
        name: records::string_or_empty(record.get("name")),
        state_label: state_label(&state),
        state,
        project_name: project
            .as_ref()
            .map(|(_, name)| name.clone())
            .unwrap_or_default(),
        project_id: project.map(|(id, _)| id).unwrap_or_default(),
        stage_name: stage
            .map(|(_, name)| name)
            .unwrap_or_else(|| DEPLOY_STAGE.to_string()),
        priority: records::optional_string(record.get("priority")),
        deadline: records::optional_string(record.get("date_deadline")),
        branch: records::string_or_empty(record.get("gitlab_branch_name")),
        // Odoo's cached MR state — replaced by the live one when it lands.
        mr_state: records::string_or_empty(record.get("gitlab_merge_request_state")),
        mr_iid: parsed.as_ref().map(|reference| reference.iid).or_else(|| {
            record
                .get("gitlab_merge_request_iid")
                .and_then(Value::as_i64)
                .filter(|iid| *iid > 0)
        }),
        mr_project_path: parsed
            .map(|reference| reference.project_path)
            .unwrap_or_default(),
        mr_url,
        mr: None,
        mr_error: None,
    }
}

/// Odoo sometimes records a merge request's iid but not its web URL, which
/// leaves the task looking MR-less even though the MR exists. The project's
/// linked GitLab repository supplies the missing path.
///
/// One lookup per project, and only when at least one of its tasks needs it —
/// and only when the project maps to EXACTLY one repository: with several there
/// is no way to tell which one the iid belongs to, so it is left unresolved
/// rather than guessed into the wrong repo.
fn fill_missing_mr_paths(odoo: &OdooClient, board: &mut DeployBoard) {
    for project in board.projects.values_mut() {
        let orphans = project
            .tasks
            .iter()
            .any(|task| task.mr_iid.is_some() && task.mr_project_path.is_empty());
        if !orphans || project.project_id == 0 {
            continue;
        }
        let paths = odoo.get_project_repo_paths(project.project_id);
        let [path] = paths.as_slice() else {
            continue;
        };
        for task in &mut project.tasks {
            if task.mr_iid.is_some() && task.mr_project_path.is_empty() {
                // Only the path: the canonical URL arrives with the live MR
                // record, so there is no need to guess the web host.
                task.mr_project_path.clone_from(path);
            }
        }
    }
}

/// Overlay live GitLab state onto every task that has a merge request.
///
/// Best-effort by design: a failure leaves Odoo's stale value in place and
/// records the reason on the row, because a board with one unreadable MR is
/// still the board.
pub fn enrich_with_live_mrs(gitlab: &Gitlab, board: &mut DeployBoard) {
    for project in board.projects.values_mut() {
        for task in &mut project.tasks {
            let (Some(iid), false) = (task.mr_iid, task.mr_project_path.is_empty()) else {
                continue;
            };
            match gitlab.fetch_mr(&task.mr_project_path, iid) {
                Ok(mr) => {
                    // Live wins: Odoo's cached state only updates when the
                    // integration syncs, so it is routinely a merge behind.
                    task.mr_state.clone_from(&mr.state);
                    if task.mr_url.is_empty() {
                        task.mr_url.clone_from(&mr.url);
                    }
                    task.mr = Some(mr);
                    task.mr_error = None;
                }
                Err(error) => task.mr_error = Some(error.to_string()),
            }
        }
    }
}
