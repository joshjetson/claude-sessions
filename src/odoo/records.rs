//! Turning raw `project.task` records into the domain types.
//!
//! Odoo's JSON is loosely typed on purpose: an unset many2one comes back as
//! `false`, not `null`, and a Studio field may be either an integer or the
//! selection string mirroring it. Every one of those shapes is decoded here so
//! nothing above this file has to know about them.

use serde_json::Value;

use crate::types::Task;

/// The fields every board query asks for, verbatim from the Node original.
///
/// `depend_on_count` versus `closed_depend_on_count` is what tells us whether a
/// task's blockers have actually landed — the count alone cannot.
pub const TASK_FIELDS: &[&str] = &[
    "id",
    "name",
    "stage_id",
    "project_id",
    "priority",
    "date_deadline",
    "tag_ids",
    "x_studio_story_points_1",
    "x_studio_story_points",
    "parent_id",
    "child_ids",
    "depend_on_ids",
    "depend_on_count",
    "closed_depend_on_count",
];

/// The name shown when a task sits in no stage at all.
pub const NO_STAGE: &str = "No stage";

/// A task blocking another one, with enough detail to explain the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub id: i64,
    pub name: String,
    pub stage_name: String,
    /// Closed in Odoo's dependency sense — done, cancelled or complete.
    pub done: bool,
}

/// What the task detail pane shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetail {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub stage_name: String,
    pub project_name: String,
    pub priority: String,
    pub deadline: Option<String>,
}

/// The GitLab fields the Odoo integration writes onto a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskGitlab {
    pub branch_name: String,
    pub merge_request_url: String,
    pub merge_request_state: String,
}

/// An Odoo project, as the project picker and the board filters see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdooProject {
    pub id: i64,
    pub name: String,
}

/// `[id, name]` or `false` — Odoo's many2one encoding.
pub fn many2one(value: Option<&Value>) -> Option<(i64, String)> {
    let pair = value?.as_array()?;
    let id = pair.first()?.as_i64()?;
    let name = pair.get(1)?.as_str()?.to_string();
    Some((id, name))
}

/// A `false`-or-string field, with `false` and `""` both meaning absent.
pub fn optional_string(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

pub fn string_or_empty(value: Option<&Value>) -> String {
    optional_string(value).unwrap_or_default()
}

pub fn id_list(value: Option<&Value>) -> Vec<i64> {
    value
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

pub fn count(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_i64)
        .map(|n| n.max(0) as u32)
        .unwrap_or(0)
}

/// Build the board task from a raw record. `tag_names` resolves tag ids, which
/// the board fetches once per refresh rather than per task.
pub fn to_task(record: &Value, tag_names: &dyn Fn(i64) -> Option<String>) -> Task {
    let (project_id, project_name) =
        many2one(record.get("project_id")).unwrap_or((0, String::new()));
    let (stage_id, stage_name) =
        many2one(record.get("stage_id")).unwrap_or((0, NO_STAGE.to_string()));
    let blocker_count = count(record.get("depend_on_count"));
    let closed_blockers = count(record.get("closed_depend_on_count"));

    Task {
        id: record.get("id").and_then(Value::as_i64).unwrap_or(0),
        name: string_or_empty(record.get("name")),
        priority: optional_string(record.get("priority")),
        deadline: optional_string(record.get("date_deadline")),
        stage_id,
        stage_name,
        project_id,
        project_name,
        tags: id_list(record.get("tag_ids"))
            .into_iter()
            .filter_map(tag_names)
            .collect(),
        story_points: story_points_of(record),
        parent_id: many2one(record.get("parent_id")).map(|(id, _)| id),
        child_ids: id_list(record.get("child_ids")),
        subtasks: Vec::new(),
        blocked_by: id_list(record.get("depend_on_ids")),
        blocker_count,
        open_blocker_count: blocker_count.saturating_sub(closed_blockers),
    }
}

/// Story points live in a Studio field: the integer `x_studio_story_points_1`,
/// mirrored by the selection string `x_studio_story_points`. Prefer the
/// integer, fall back to the selection, and treat anything non-positive as
/// unset — an unestimated task must not read as "0 points".
pub fn story_points_of(record: &Value) -> Option<i64> {
    let numeric = record
        .get("x_studio_story_points_1")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0);
    if numeric.is_some() {
        return numeric;
    }
    let selection = record
        .get("x_studio_story_points")
        .and_then(Value::as_str)?;
    leading_int(selection).filter(|n| *n > 0)
}

/// JavaScript's `parseInt` semantics: leading whitespace and sign, then digits,
/// stopping at the first non-digit — so "3 points" is 3 and "small" is nothing.
fn leading_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let taken: String = digits.chars().take_while(char::is_ascii_digit).collect();
    taken.parse::<i64>().ok().map(|n| n * sign)
}

pub fn to_blocker(record: &Value) -> Blocker {
    Blocker {
        id: record.get("id").and_then(Value::as_i64).unwrap_or(0),
        name: string_or_empty(record.get("name")),
        stage_name: many2one(record.get("stage_id"))
            .map(|(_, name)| name)
            .unwrap_or_else(|| NO_STAGE.to_string()),
        done: record
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| super::stages::CLOSED_STATES.contains(&state)),
    }
}

pub fn to_detail(record: &Value) -> TaskDetail {
    TaskDetail {
        id: record.get("id").and_then(Value::as_i64).unwrap_or(0),
        name: string_or_empty(record.get("name")),
        description: string_or_empty(record.get("description")),
        stage_name: many2one(record.get("stage_id"))
            .map(|(_, name)| name)
            .unwrap_or_else(|| NO_STAGE.to_string()),
        project_name: many2one(record.get("project_id"))
            .map(|(_, name)| name)
            .unwrap_or_default(),
        priority: string_or_empty(record.get("priority")),
        deadline: optional_string(record.get("date_deadline")),
    }
}
