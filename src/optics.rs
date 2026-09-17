//! Optics client — read-only coverage lookups over a PostgREST API.
//!
//! Ported from the Node app's `src/optics.js`. Optics records UI "processes" —
//! the routes, clicked elements, source files, API calls and form inputs a
//! workflow touches — into Postgres, exposed via PostgREST. Recordings are
//! grouped into categories, and the team convention is that a category named
//! `Task-<odooTaskId>` holds the recordings for that Odoo task. This module
//! answers "does task N have recorded coverage?" for the board's 🔬N badge.
//!
//! Endpoint and token are REQUIRED configuration with no defaults. The Node
//! original shipped both a default API host and a live token in source; neither
//! carries over to a public build.
//!
//! Coverage is a nice-to-have, so every error path here resolves to "no
//! coverage" rather than an error the board has to render.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::util::normalise_name;

/// One request may take this long. The board must not wait on Optics.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// One GET against the Optics API, returning the response body.
///
/// A trait of its own rather than [`crate::odoo::Transport`]: that one is
/// `post_json(url, body)` because Odoo is JSON-RPC over POST, and widening it
/// to carry a method and headers would make every implementor — including the
/// Odoo test stub — grow a case it can never serve. Two transports, each the
/// shape of the protocol it speaks.
pub trait OpticsHttp: Send + Sync {
    fn get(&self, url: &str, token: &str) -> Result<String, String>;
}

/// The real transport: the same pooled-agent-with-a-global-timeout pattern the
/// Odoo client uses.
pub struct HttpGet {
    agent: ureq::Agent,
}

impl HttpGet {
    pub fn new(timeout: Duration) -> Self {
        HttpGet {
            agent: crate::http::agent(timeout, false),
        }
    }
}

impl Default for HttpGet {
    fn default() -> Self {
        HttpGet::new(DEFAULT_TIMEOUT)
    }
}

impl OpticsHttp for HttpGet {
    fn get(&self, url: &str, token: &str) -> Result<String, String> {
        let mut response = self
            .agent
            .get(url)
            .header("X-Optics-Token", token)
            .call()
            .map_err(|err| err.to_string())?;
        let status = response.status();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|err| err.to_string())?;
        if !status.is_success() {
            return Err(format!("Optics API {status}"));
        }
        Ok(body)
    }
}

/// An Optics project as the API lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OpticsProject {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sdk_key: String,
}

/// One recorded process inside a task's category.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OpticsProcess {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CategoryRow {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    processes: Vec<OpticsProcess>,
}

/// Coverage detail for a single task — the detail pane's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOptics {
    pub project_sdk_key: String,
    pub category: String,
    pub category_id: i64,
    pub processes: Vec<OpticsProcess>,
}

impl TaskOptics {
    pub fn count(&self) -> usize {
        self.processes.len()
    }
}

/// The client. Holds the project list once it has been fetched — that list is
/// small and changes about as often as a new client is onboarded.
pub struct OpticsClient {
    http: Box<dyn OpticsHttp>,
    api: String,
    token: String,
    /// Odoo project name -> Optics sdk_key, from config.
    mapping: HashMap<String, String>,
    projects: std::sync::Mutex<Option<Vec<OpticsProject>>>,
}

impl OpticsClient {
    /// `None` when the install is not configured for Optics, which is the
    /// normal case: the badges are simply off.
    pub fn from_config(config: &crate::config::ConfigHandle) -> Option<OpticsClient> {
        let api = config.optics_api()?.to_string();
        let token = config.optics_token()?.to_string();
        let mapping = config.optics_projects().clone();
        Some(OpticsClient::new(
            Box::new(HttpGet::default()),
            api,
            token,
            mapping,
        ))
    }

    pub fn new(
        http: Box<dyn OpticsHttp>,
        api: String,
        token: String,
        mapping: HashMap<String, String>,
    ) -> Self {
        OpticsClient {
            http,
            api: api.trim_end_matches('/').to_string(),
            token,
            mapping,
            projects: std::sync::Mutex::new(None),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let body = self.http.get(&format!("{}{path}", self.api), &self.token)?;
        serde_json::from_str(&body).map_err(|err| err.to_string())
    }

    /// The project list, fetched at most once per client.
    pub fn projects(&self) -> Result<Vec<OpticsProject>, String> {
        if let Ok(cache) = self.projects.lock() {
            if let Some(projects) = cache.as_ref() {
                return Ok(projects.clone());
            }
        }
        let projects: Vec<OpticsProject> = self.get("/projects?select=id,name,sdk_key")?;
        if let Ok(mut cache) = self.projects.lock() {
            *cache = Some(projects.clone());
        }
        Ok(projects)
    }

    /// Resolve an Odoo project name to the Optics project holding its
    /// recordings.
    ///
    /// An explicit `optics.projects` mapping wins; otherwise a fuzzy match on
    /// the normalised name or sdk_key — which is what makes a long Odoo project
    /// name resolve to a short Optics key without configuration.
    pub fn resolve_project(&self, odoo_project: &str) -> Result<Option<OpticsProject>, String> {
        if odoo_project.is_empty() {
            return Ok(None);
        }
        let list = self.projects()?;
        Ok(match_project(&list, &self.mapping, odoo_project))
    }

    /// Coverage detail for one task, or `None` when its project is not on
    /// Optics or has no `Task-<id>` category with at least one process.
    pub fn task_optics(
        &self,
        task_id: i64,
        odoo_project: &str,
    ) -> Result<Option<TaskOptics>, String> {
        let Some(project) = self.resolve_project(odoo_project)? else {
            return Ok(None);
        };
        let rows: Vec<CategoryRow> = self.get(&format!(
            "/process_categories?project_id=eq.{}&name=eq.{}&select=id,name,processes(id,name,actor,description)",
            project.id,
            encode(&category_name(task_id)),
        ))?;
        let Some(row) = rows.into_iter().next().filter(|r| !r.processes.is_empty()) else {
            return Ok(None);
        };
        Ok(Some(TaskOptics {
            project_sdk_key: project.sdk_key,
            category: row.name,
            category_id: row.id,
            processes: row.processes,
        }))
    }

    /// Board coverage: task id -> recorded process count, for the tasks that
    /// have any.
    ///
    /// ONE query per project — the `name=in.(…)` filter is the whole point of
    /// the endpoint shape. An empty map on any error: coverage must never break
    /// board rendering.
    pub fn project_coverage(&self, odoo_project: &str, task_ids: &[i64]) -> HashMap<i64, usize> {
        if task_ids.is_empty() {
            return HashMap::new();
        }
        self.try_project_coverage(odoo_project, task_ids)
            .unwrap_or_default()
    }

    fn try_project_coverage(
        &self,
        odoo_project: &str,
        task_ids: &[i64],
    ) -> Result<HashMap<i64, usize>, String> {
        let Some(project) = self.resolve_project(odoo_project)? else {
            return Ok(HashMap::new());
        };
        let names = task_ids
            .iter()
            .map(|id| format!("\"{}\"", category_name(*id)))
            .collect::<Vec<_>>()
            .join(",");
        let rows: Vec<CategoryRow> = self.get(&format!(
            "/process_categories?project_id=eq.{}&name=in.({})&select=name,processes(id)",
            project.id,
            encode(&names),
        ))?;
        Ok(rows
            .into_iter()
            .filter(|row| !row.processes.is_empty())
            .filter_map(|row| Some((task_id_of(&row.name)?, row.processes.len())))
            .collect())
    }
}

/// The Optics category name convention for an Odoo task.
pub fn category_name(task_id: i64) -> String {
    format!("Task-{task_id}")
}

/// `Task-5944` -> `5944`, and nothing else.
fn task_id_of(category: &str) -> Option<i64> {
    let digits = category.strip_prefix("Task-")?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Explicit mapping first, then fuzzy — split out so it can be tested without a
/// transport.
pub fn match_project(
    list: &[OpticsProject],
    mapping: &HashMap<String, String>,
    odoo_project: &str,
) -> Option<OpticsProject> {
    let wanted = normalise_name(odoo_project);
    // 1) explicit config: Odoo project name -> Optics sdk_key (or name).
    let configured = mapping
        .iter()
        .find(|(key, _)| normalise_name(key) == wanted)
        .map(|(_, value)| normalise_name(value));
    if let Some(configured) = configured {
        if let Some(hit) = list.iter().find(|p| {
            normalise_name(&p.sdk_key) == configured || normalise_name(&p.name) == configured
        }) {
            return Some(hit.clone());
        }
    }
    // 2) heuristic: exact sdk_key/name, then substring either direction.
    list.iter()
        .find(|p| normalise_name(&p.sdk_key) == wanted || normalise_name(&p.name) == wanted)
        .or_else(|| {
            list.iter()
                .find(|p| !p.sdk_key.is_empty() && wanted.contains(&normalise_name(&p.sdk_key)))
        })
        .or_else(|| {
            list.iter()
                .find(|p| !p.name.is_empty() && wanted.contains(&normalise_name(&p.name)))
        })
        .cloned()
}

/// Percent-encode a PostgREST query value. Only the characters that would
/// otherwise end the value or the query need escaping; the crate carries no URL
/// dependency and this is the one place that needs any encoding at all.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests;
