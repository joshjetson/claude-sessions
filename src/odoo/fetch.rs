//! Fetching the board: the one query the dashboard runs every refresh, and the
//! grouping that turns its rows into projects, stages and nested subtasks.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::records::{self, to_task, TASK_FIELDS};
use super::{OdooClient, Result};
use crate::types::{Board, BoardProject, BoardStage, Task};

/// How the board query is scoped. Defaults match the Node signature: my tasks,
/// no project filter, 800-task ceiling for the unscoped "all" view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchBoardOptions {
    /// Only tasks assigned to the authenticated user.
    pub mine_only: bool,
    /// "All" view: project names to load; empty means every project.
    pub include: Vec<String>,
    /// Project names to exclude from both views.
    pub ignore: Vec<String>,
    /// Stage names hidden from the board (e.g. "Deployed").
    pub hide_stages: Vec<String>,
    /// `state` values hidden from the board (e.g. done and cancelled).
    pub hide_states: Vec<String>,
    /// Cap for the unscoped "all" view only.
    pub limit: usize,
}

impl Default for FetchBoardOptions {
    fn default() -> Self {
        FetchBoardOptions {
            mine_only: true,
            include: Vec::new(),
            ignore: Vec::new(),
            hide_stages: Vec::new(),
            hide_states: Vec::new(),
            limit: 800,
        }
    }
}

impl FetchBoardOptions {
    /// The query this install is configured for.
    ///
    /// Written once because two callers ask Odoo the same question: the
    /// dashboard when it fetches the board itself, and the daemon's 45-second
    /// poll. A board whose shape depended on which of them fetched it would be
    /// a filter bug nobody could reproduce.
    pub fn for_config(config: &crate::config::ConfigHandle, mine_only: bool) -> Self {
        let projects = config.board_project_filter();
        let hide = config.board_hide_filter();
        FetchBoardOptions {
            mine_only,
            include: projects.include,
            ignore: projects.ignore,
            hide_stages: hide.hide_stages,
            hide_states: hide.hide_states,
            ..FetchBoardOptions::default()
        }
    }
}

impl OdooClient {
    // --- the board ----------------------------------------------------------

    /// Tasks grouped into `project -> stage -> tasks`, with subtasks nested.
    pub fn fetch_board(&self, options: &FetchBoardOptions) -> Result<Board> {
        let uid = self.authenticate()?;

        let mut domain: Vec<Value> = Vec::new();
        if options.mine_only {
            domain.push(json!(["user_ids", "=", uid]));
        }
        // Hide tasks in unwanted states (by default Done and Cancelled).
        // "Complete" (03_approved) is deliberately kept.
        if !options.hide_states.is_empty() {
            domain.push(json!(["state", "not in", options.hide_states]));
        }

        // Scope the all-view to an allowlist of projects when configured.
        let mut scoped = false;
        if !options.mine_only && !options.include.is_empty() {
            let mut ids = self.project_ids_for_names(&options.include)?;
            if ids.is_empty() {
                // A name that matches nothing must return nothing, not
                // everything — id 0 exists nowhere.
                ids.push(0);
            }
            domain.push(json!(["project_id", "in", ids]));
            scoped = true;
        }
        if !options.ignore.is_empty() {
            let ids = self.project_ids_for_names(&options.ignore)?;
            if !ids.is_empty() {
                domain.push(json!(["project_id", "not in", ids]));
            }
        }

        let records = self.search_read(
            "project.task",
            domain,
            json!({
                "fields": TASK_FIELDS,
                "order": "write_date desc",
                // 0 means "no limit": my own board and an explicitly scoped one
                // are already bounded by the filter.
                "limit": if options.mine_only || scoped { 0 } else { options.limit as i64 },
            }),
        )?;

        let stage_meta = self.stage_meta()?;
        let tag_meta = self.tag_meta();
        let tag_name = |id: i64| tag_meta.get(&id).cloned();

        let hidden_stages: HashSet<String> = options
            .hide_stages
            .iter()
            .map(|name| name.to_lowercase())
            .collect();

        let mut board = Board {
            task_count: records.len(),
            truncated: !options.mine_only && !scoped && records.len() >= options.limit,
            projects: Default::default(),
        };

        // Which records made it onto the board, in order, so subtasks can be
        // attached to exactly those.
        let visible: Vec<&Value> = records
            .iter()
            .filter(|record| {
                record
                    .get("project_id")
                    .and_then(|p| p.as_array())
                    .is_some()
            })
            .filter(|record| !is_hidden_stage(record, &hidden_stages))
            .collect();

        let children = self.fetch_children(&visible, &options.hide_states);
        let child_tasks: HashMap<i64, Task> = children
            .iter()
            .filter(|record| !is_hidden_stage(record, &hidden_stages))
            .map(|record| {
                let task = to_task(record, &tag_name);
                (task.id, task)
            })
            .collect();

        let mut nested: HashSet<i64> = HashSet::new();
        for record in &visible {
            for child_id in records::id_list(record.get("child_ids")) {
                if child_tasks.contains_key(&child_id) {
                    nested.insert(child_id);
                }
            }
        }

        for record in &visible {
            let mut task = to_task(record, &tag_name);
            for child_id in records::id_list(record.get("child_ids")) {
                if let Some(child) = child_tasks.get(&child_id) {
                    task.subtasks.push(child.clone());
                }
            }
            task.subtasks.sort_by_key(|sub| sub.id);

            let project = board
                .projects
                .entry(task.project_name.clone())
                .or_insert_with(|| BoardProject {
                    project_id: task.project_id,
                    stages: Default::default(),
                });
            let stage = project
                .stages
                .entry(task.stage_name.clone())
                .or_insert_with(|| BoardStage {
                    stage_id: task.stage_id,
                    sequence: stage_meta
                        .get(&task.stage_id)
                        .map(|meta| meta.sequence)
                        .unwrap_or(999),
                    tasks: Vec::new(),
                });
            stage.tasks.push(task);
        }

        // A subtask that also matched the top-level filter would show twice —
        // drop it from the top level now that it hangs under its parent. The
        // stage itself stays even if that empties it, exactly as it did before.
        if !nested.is_empty() {
            for project in board.projects.values_mut() {
                for stage in project.stages.values_mut() {
                    stage.tasks.retain(|task| !nested.contains(&task.id));
                }
            }
        }

        Ok(board)
    }

    /// Children of the visible tasks, fetched by id because they are usually
    /// assigned to somebody else and so never come back in the board query.
    /// Best-effort: a failure here loses nesting, not the board.
    fn fetch_children(&self, visible: &[&Value], hide_states: &[String]) -> Vec<Value> {
        let child_ids: Vec<i64> = visible
            .iter()
            .flat_map(|record| records::id_list(record.get("child_ids")))
            .collect::<HashSet<i64>>()
            .into_iter()
            .collect();
        if child_ids.is_empty() {
            return Vec::new();
        }

        let mut domain = vec![json!(["id", "in", child_ids])];
        if !hide_states.is_empty() {
            domain.push(json!(["state", "not in", hide_states]));
        }
        self.search_read("project.task", domain, json!({ "fields": TASK_FIELDS }))
            .unwrap_or_default()
    }

    /// Tasks assigned to me sitting in any of the given stages.
    ///
    /// Deliberately independent of the board query: "am I assigned something
    /// new" must not change meaning because someone switched the view to "all".
    pub fn fetch_assigned_in_stages(&self, stage_names: &[String]) -> Result<Vec<Task>> {
        if stage_names.is_empty() {
            return Ok(Vec::new());
        }
        let uid = self.authenticate()?;
        let records = self.search_read(
            "project.task",
            vec![
                json!(["user_ids", "=", uid]),
                json!(["stage_id.name", "in", stage_names]),
                json!([
                    "state",
                    "not in",
                    [super::task_state::DONE, super::task_state::CANCELLED]
                ]),
            ],
            json!({ "fields": TASK_FIELDS, "order": "write_date desc", "limit": 200 }),
        )?;
        let tag_meta = self.tag_meta();
        Ok(records
            .iter()
            .filter(|record| {
                record
                    .get("project_id")
                    .and_then(|p| p.as_array())
                    .is_some()
            })
            .map(|record| to_task(record, &|id| tag_meta.get(&id).cloned()))
            .collect())
    }
}

/// A task sitting in a QA stage, with what the QA arrival rule needs beyond
/// the board fields: who it is assigned to, and when it entered its stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaStageTask {
    pub task: Task,
    /// Odoo user ids on the task.
    pub user_ids: Vec<i64>,
    /// Whether the authenticated user is one of `user_ids`.
    pub assigned_to_me: bool,
    /// Odoo's `date_last_stage_update`, as written. Empty when Odoo sent none.
    ///
    /// It changes each time the task enters a stage, so a task that goes to QA,
    /// back to a developer and to QA again reads as a new arrival the second
    /// time, while a daemon restart does not.
    pub stage_entered: String,
}

impl OdooClient {
    /// Every open task in the given stages, whoever it is assigned to.
    ///
    /// Not scoped to the current user, unlike
    /// [`OdooClient::fetch_assigned_in_stages`]: a QA reviewer is told about
    /// unclaimed arrivals too, and a task claimed by another reviewer is told
    /// apart by its `user_ids`. Newest stage entry first, so the 200-row cap
    /// drops the oldest arrivals rather than the newest.
    ///
    /// "Complete" (`03_approved`) is excluded along with done and cancelled. A
    /// developer can mark a task complete while its stage still says QA, and
    /// the QA Board found tasks left that way for 195 days.
    pub fn fetch_in_qa_stages(&self, stage_names: &[String]) -> Result<Vec<QaStageTask>> {
        if stage_names.is_empty() {
            return Ok(Vec::new());
        }
        let uid = self.authenticate()?;
        let mut fields: Vec<&str> = TASK_FIELDS.to_vec();
        fields.extend(["user_ids", "date_last_stage_update"]);
        let records = self.search_read(
            "project.task",
            vec![
                json!(["stage_id.name", "in", stage_names]),
                json!([
                    "state",
                    "not in",
                    [
                        super::task_state::DONE,
                        super::task_state::CANCELLED,
                        super::task_state::COMPLETE
                    ]
                ]),
            ],
            json!({ "fields": fields, "order": "date_last_stage_update desc", "limit": 200 }),
        )?;
        let tag_meta = self.tag_meta();
        Ok(records
            .iter()
            .filter(|record| {
                record
                    .get("project_id")
                    .and_then(|p| p.as_array())
                    .is_some()
            })
            .map(|record| {
                let user_ids = records::id_list(record.get("user_ids"));
                QaStageTask {
                    task: to_task(record, &|id| tag_meta.get(&id).cloned()),
                    assigned_to_me: user_ids.contains(&uid),
                    user_ids,
                    stage_entered: record
                        .get("date_last_stage_update")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }
            })
            .collect())
    }
}

/// The rows of a `read`/`search_read` result, or nothing when the shape is
/// not what we asked for.
pub(super) fn as_records(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map(|rows| rows.iter().collect())
        .unwrap_or_default()
}

fn is_hidden_stage(record: &Value, hidden: &HashSet<String>) -> bool {
    if hidden.is_empty() {
        return false;
    }
    let stage = records::many2one(record.get("stage_id"))
        .map(|(_, name)| name)
        .unwrap_or_else(|| records::NO_STAGE.to_string());
    hidden.contains(&stage.to_lowercase())
}
