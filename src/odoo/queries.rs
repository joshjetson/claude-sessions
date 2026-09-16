//! Everything else the dashboard asks Odoo for: blockers, stages, task detail,
//! the GitLab links, the writes, and the cached metadata lookups.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::fetch::as_records;
use super::records::{self, to_blocker, to_detail, Blocker, OdooProject, TaskDetail, TaskGitlab};
use super::stages::{resolve_stage, StageKind, StageMatch, StageRecord};
use super::{OdooClient, OdooError, Result, StageMeta};

impl OdooClient {
    /// The tasks blocking `task_ids`, with enough detail to explain the block.
    pub fn fetch_blockers(&self, task_ids: &[i64]) -> Result<Vec<Blocker>> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.execute_kw(
            "project.task",
            "read",
            vec![json!(task_ids)],
            json!({ "fields": ["id", "name", "stage_id", "state"] }),
        )?;
        Ok(as_records(&rows).into_iter().map(to_blocker).collect())
    }

    /// Stage name for each of `task_ids`, in one call.
    ///
    /// The board only holds what the current filter shows, so a session working
    /// a task that has moved on has no stage there. The purge needs the real
    /// answer rather than reading absence as "finished".
    pub fn fetch_stages_for_tasks(&self, task_ids: &[i64]) -> Result<HashMap<i64, String>> {
        let ids: Vec<i64> = task_ids
            .iter()
            .copied()
            .filter(|id| *id != 0)
            .collect::<HashSet<i64>>()
            .into_iter()
            .collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self.execute_kw(
            "project.task",
            "read",
            vec![json!(ids)],
            json!({ "fields": ["id", "stage_id"] }),
        )?;
        Ok(as_records(&rows)
            .iter()
            .filter_map(|record| {
                let id = record.get("id").and_then(Value::as_i64)?;
                let stage = records::many2one(record.get("stage_id"))
                    .map(|(_, name)| name)
                    .unwrap_or_else(|| records::NO_STAGE.to_string());
                Some((id, stage))
            })
            .collect())
    }

    pub fn get_task_detail(&self, task_id: i64) -> Result<Option<TaskDetail>> {
        let rows = self.execute_kw(
            "project.task",
            "read",
            vec![json!([task_id])],
            json!({
                "fields": ["id", "name", "description", "stage_id", "project_id", "priority", "date_deadline"],
            }),
        )?;
        Ok(as_records(&rows).first().map(|record| to_detail(record)))
    }

    /// The GitLab fields the Odoo integration writes onto a task. Best-effort:
    /// the integration is not installed everywhere, so a failure reads as "no
    /// GitLab information" rather than breaking the dialog.
    pub fn get_task_gitlab(&self, task_id: i64) -> Option<TaskGitlab> {
        let rows = self
            .execute_kw(
                "project.task",
                "read",
                vec![json!([task_id])],
                json!({
                    "fields": ["gitlab_branch_name", "gitlab_merge_request_url", "gitlab_merge_request_state"],
                }),
            )
            .ok()?;
        let record = as_records(&rows).first().cloned()?;
        Some(TaskGitlab {
            branch_name: records::string_or_empty(record.get("gitlab_branch_name")),
            merge_request_url: records::string_or_empty(record.get("gitlab_merge_request_url")),
            merge_request_state: records::string_or_empty(record.get("gitlab_merge_request_state")),
        })
    }

    // --- writes -------------------------------------------------------------

    pub fn move_stage(&self, task_id: i64, stage_id: i64) -> Result<()> {
        self.write_task(task_id, json!({ "stage_id": stage_id }))
    }

    /// Set a task's state (e.g. [`super::task_state::COMPLETE`]). Leaves its
    /// stage untouched — they are separate axes.
    pub fn set_task_state(&self, task_id: i64, state: &str) -> Result<()> {
        self.write_task(task_id, json!({ "state": state }))
    }

    /// Post to the task's chatter. `body_is_html` matters: without it Odoo
    /// renders the markup as literal text.
    pub fn post_comment(&self, task_id: i64, html: &str) -> Result<()> {
        self.execute_kw(
            "project.task",
            "message_post",
            vec![json!([task_id])],
            json!({ "body": html, "body_is_html": true, "message_type": "comment" }),
        )
        .map(|_| ())
    }

    fn write_task(&self, task_id: i64, values: Value) -> Result<()> {
        self.execute_kw(
            "project.task",
            "write",
            vec![json!([task_id]), values],
            json!({}),
        )
        .map(|_| ())
    }

    // --- projects, tags, stages --------------------------------------------

    /// All projects as `{id, name}`, cached until [`OdooClient::invalidate_metadata`].
    pub fn get_projects(&self) -> Result<Vec<OdooProject>> {
        if let Some(projects) = self.with_caches(|caches| caches.projects.clone()) {
            return Ok(projects);
        }
        let rows = self.search_read(
            "project.project",
            Vec::new(),
            json!({ "fields": ["id", "name"], "order": "name" }),
        )?;
        let projects: Vec<OdooProject> = rows
            .iter()
            .filter_map(|record| {
                Some(OdooProject {
                    id: record.get("id").and_then(Value::as_i64)?,
                    name: records::string_or_empty(record.get("name")),
                })
            })
            .collect();
        self.with_caches(|caches| caches.projects = Some(projects.clone()));
        Ok(projects)
    }

    pub(super) fn project_ids_for_names(&self, names: &[String]) -> Result<Vec<i64>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
        Ok(self
            .get_projects()?
            .into_iter()
            .filter(|project| wanted.contains(project.name.as_str()))
            .map(|project| project.id)
            .collect())
    }

    /// Tag id to name. Best-effort and cached: tags decorate rows, so a failure
    /// costs labels, not the board.
    pub fn tag_meta(&self) -> HashMap<i64, String> {
        if let Some(tags) = self.with_caches(|caches| caches.tags.clone()) {
            return tags;
        }
        let tags: HashMap<i64, String> = self
            .search_read(
                "project.tags",
                Vec::new(),
                json!({ "fields": ["id", "name"] }),
            )
            .unwrap_or_default()
            .iter()
            .filter_map(|record| {
                Some((
                    record.get("id").and_then(Value::as_i64)?,
                    records::string_or_empty(record.get("name")),
                ))
            })
            .collect();
        self.with_caches(|caches| caches.tags = Some(tags.clone()));
        tags
    }

    /// Stage id to name and sequence, across every project. Cached.
    pub fn stage_meta(&self) -> Result<HashMap<i64, StageMeta>> {
        if let Some(stages) = self.with_caches(|caches| caches.stages.clone()) {
            return Ok(stages);
        }
        let rows = self.search_read(
            "project.task.type",
            Vec::new(),
            json!({ "fields": ["id", "name", "sequence"] }),
        )?;
        let stages: HashMap<i64, StageMeta> = rows
            .iter()
            .filter_map(|record| {
                Some((
                    record.get("id").and_then(Value::as_i64)?,
                    StageMeta {
                        name: records::string_or_empty(record.get("name")),
                        sequence: record.get("sequence").and_then(Value::as_i64).unwrap_or(0),
                    },
                ))
            })
            .collect();
        self.with_caches(|caches| caches.stages = Some(stages.clone()));
        Ok(stages)
    }

    /// The stages that apply to one project, ordered by sequence.
    pub fn get_project_stages(&self, project_id: i64) -> Result<Vec<StageRecord>> {
        let rows = self.search_read(
            "project.task.type",
            vec![json!(["project_ids", "in", [project_id]])],
            json!({ "fields": ["id", "name", "sequence"], "order": "sequence" }),
        )?;
        Ok(rows
            .iter()
            .filter_map(|record| {
                Some(StageRecord {
                    id: record.get("id").and_then(Value::as_i64)?,
                    name: records::string_or_empty(record.get("name")),
                    sequence: record.get("sequence").and_then(Value::as_i64).unwrap_or(0),
                })
            })
            .collect())
    }

    /// The stage a task should move into, or `None` to leave it alone.
    ///
    /// One entry point for both directions — [`StageKind`] carries the name
    /// list and the fuzzy fallback. `preferred` is the configured override.
    pub fn resolve_stage_for_project(
        &self,
        project_id: i64,
        kind: StageKind,
        preferred: &[String],
    ) -> Result<Option<StageMatch>> {
        let stages = self.get_project_stages(project_id)?;
        Ok(resolve_stage(&stages, kind, preferred))
    }

    // --- GitLab links stored on the Odoo project ----------------------------

    /// Repo paths linked to an Odoo project. Used to recover an MR's project
    /// path when Odoo stored the iid but not the URL.
    pub fn get_project_repo_paths(&self, project_id: i64) -> Vec<String> {
        self.linked_repository_field(project_id, "full_path")
    }

    /// Default branch of the project's linked repo (e.g. development / main).
    pub fn get_project_default_branch(&self, project_id: i64) -> Option<String> {
        self.linked_repository_field(project_id, "default_branch")
            .into_iter()
            .next()
    }

    /// One field off every GitLab repository linked to an Odoo project. Both
    /// callers above want the same two hops, so the hops live here once.
    fn linked_repository_field(&self, project_id: i64, field: &str) -> Vec<String> {
        let Ok(project) = self.execute_kw(
            "project.project",
            "read",
            vec![json!([project_id])],
            json!({ "fields": ["gitlab_repository_ids"] }),
        ) else {
            return Vec::new();
        };
        let repo_ids = as_records(&project)
            .first()
            .map(|record| records::id_list(record.get("gitlab_repository_ids")))
            .unwrap_or_default();
        if repo_ids.is_empty() {
            return Vec::new();
        }
        let Ok(repos) = self.execute_kw(
            "gitlab.repository",
            "read",
            vec![json!(repo_ids)],
            json!({ "fields": [field] }),
        ) else {
            return Vec::new();
        };
        as_records(&repos)
            .iter()
            .filter_map(|record| records::optional_string(record.get(field)))
            .collect()
    }

    pub(super) fn search_read(
        &self,
        model: &str,
        domain: Vec<Value>,
        kwargs: Value,
    ) -> Result<Vec<Value>> {
        let rows = self.execute_kw(model, "search_read", vec![Value::Array(domain)], kwargs)?;
        match rows {
            Value::Array(records) => Ok(records),
            other => Err(OdooError::BadResponse(format!(
                "{model}.search_read returned {other}"
            ))),
        }
    }
}
