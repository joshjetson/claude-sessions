//! The two knowledge stores that are otherwise write-only, joined.
//!
//! Ported from the Node app's `src/journal.js`:
//!   - per-repo `<repo>/docs/agent/problem-reasoning-journal.md` + `project-rules.md`
//!   - per-task `~/.claude-sessions/tasks/<taskId>/{meta.json,<sessionId>.jsonl}`
//!
//! They are joined on the Odoo task id that journal titles already carry
//! (`## [2026-07-23] Task-5944 — …`), so one entry can show its reasoning, its
//! spawn prompt, its archived transcript and its standup line together.
//!
//! Split into [`entries`] (the markdown parsers) and [`tasks`] (the archive
//! side); this file is discovery and assembly.

pub mod classify;
pub mod entries;
pub mod markdown;
pub mod tasks;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::ConfigHandle;
use crate::db::Db;
use crate::paths::Paths;

pub use classify::SectionKind;
pub use entries::{parse_journal, parse_rules, JournalEntry, JournalSection, Rule};
pub use tasks::{load_tasks, TaskArchiveView};

/// Where a repo keeps its agent documents.
pub const AGENT_DOCS_SUBDIR: &str = "docs/agent";
pub const JOURNAL_FILE: &str = "problem-reasoning-journal.md";
pub const RULES_FILE: &str = "project-rules.md";

pub fn journal_path(repo: &Path) -> PathBuf {
    repo.join(AGENT_DOCS_SUBDIR).join(JOURNAL_FILE)
}

pub fn rules_path(repo: &Path) -> PathBuf {
    repo.join(AGENT_DOCS_SUBDIR).join(RULES_FILE)
}

fn has_agent_docs(repo: &Path) -> bool {
    journal_path(repo).exists() || rules_path(repo).exists()
}

/// Repos that carry a `docs/agent/` folder.
///
/// Seeded from config (`odooProjectDirs` + groups) and from the cwd of every
/// archived task, then widened one level into each seed's parent so sibling
/// worktrees — `<project>-task-5699`, `<project>-wt-4086` — are picked up too.
pub fn discover_repos(paths: &Paths, config: &ConfigHandle) -> Vec<PathBuf> {
    let mut seeds: BTreeSet<PathBuf> = BTreeSet::new();
    for project in config.odoo_project_names() {
        for dir in config.odoo_project_dir_list(project) {
            seeds.insert(expand(paths, dir));
        }
    }
    for group in config.groups() {
        seeds.insert(expand(paths, &group.path));
    }
    for meta in tasks::list_task_metas(paths) {
        if !meta.cwd.is_empty() {
            seeds.insert(PathBuf::from(&meta.cwd));
        }
    }

    // Parents of every seed, plus the group directories themselves — a group is
    // already a folder of repos, so its children are candidates directly.
    let mut roots: BTreeSet<PathBuf> = seeds
        .iter()
        .filter_map(|seed| seed.parent().map(Path::to_path_buf))
        .collect();
    for group in config.groups() {
        roots.insert(expand(paths, &group.path));
    }

    let mut found: BTreeSet<PathBuf> = seeds
        .iter()
        .filter(|seed| seed.is_dir() && has_agent_docs(seed))
        .cloned()
        .collect();
    for root in roots {
        let Ok(children) = fs::read_dir(&root) else {
            continue;
        };
        for child in children.flatten() {
            let name = child.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !child.path().is_dir() {
                continue;
            }
            let path = child.path();
            if has_agent_docs(&path) {
                found.insert(path);
            }
        }
    }
    found.into_iter().collect()
}

fn expand(paths: &Paths, dir: &str) -> PathBuf {
    match dir.strip_prefix('~') {
        Some(rest) => paths.home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(dir),
    }
}

/// A repo's contribution, for the Repos tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    pub name: String,
    pub path: String,
    pub entries: usize,
    pub rules: usize,
    pub last_entry: Option<String>,
}

/// Where the viewer says its data came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalPaths {
    pub tasks_dir: String,
    pub prompts_dir: String,
    pub logs_dir: String,
}

/// Everything the journal viewer renders. Serialised straight into the page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalData {
    pub generated_at: String,
    pub paths: JournalPaths,
    pub repos: Vec<RepoSummary>,
    pub entries: Vec<JournalEntry>,
    pub rules: Vec<Rule>,
    pub tasks: Vec<TaskArchiveView>,
}

impl JournalData {
    /// Archived tasks that at least one journal entry references.
    pub fn linked_tasks(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| !task.entry_ids.is_empty())
            .count()
    }
}

/// Read every store and join them.
pub fn collect(paths: &Paths, config: &ConfigHandle, db: Option<&Db>) -> JournalData {
    let repo_paths = discover_repos(paths, config);
    let mut entries = Vec::new();
    let mut rules = Vec::new();
    let mut repos = Vec::new();

    for repo in &repo_paths {
        let repo_entries = parse_journal(repo);
        let repo_rules = parse_rules(repo);
        repos.push(RepoSummary {
            name: base_name(repo),
            path: repo.display().to_string(),
            entries: repo_entries.len(),
            rules: repo_rules.len(),
            last_entry: repo_entries
                .iter()
                .filter_map(|entry| entry.date.clone())
                .max(),
        });
        entries.extend(repo_entries);
        rules.extend(repo_rules);
    }

    // Newest first; a repo name breaks ties so the order is stable.
    entries.sort_by(|a, b| {
        b.date
            .as_deref()
            .unwrap_or("")
            .cmp(a.date.as_deref().unwrap_or(""))
            .then_with(|| a.repo.cmp(&b.repo))
    });

    let mut tasks = load_tasks(paths, db);
    for task in &mut tasks {
        task.entry_ids = entries
            .iter()
            .filter(|entry| entry.task_ids.contains(&task.task_id))
            .map(|entry| entry.id.clone())
            .collect();
    }

    repos.sort_by(|a, b| b.entries.cmp(&a.entries).then_with(|| a.name.cmp(&b.name)));

    JournalData {
        generated_at: crate::util::iso_now(),
        paths: JournalPaths {
            tasks_dir: paths.tasks_dir.display().to_string(),
            prompts_dir: paths.prompts_dir.display().to_string(),
            logs_dir: paths.logs_dir.display().to_string(),
        },
        repos,
        entries,
        rules,
        tasks,
    }
}

pub(crate) fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests;
