//! The task-archive half of the journal: what `~/.claude-sessions` remembers
//! about a finished task, gathered per task id so an entry can link to it.
//!
//! Three sources, all keyed on the task id: the archive directory's
//! `meta.json`, the spawn prompts that were saved when it was launched, and the
//! daily-log lines it produced. The daily-log lines come through
//! [`crate::dailylog`] rather than a parser of their own — Node had three
//! copies of that regex.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dailylog;
use crate::db::Db;
use crate::paths::Paths;

use super::base_name;

/// What the archiver wrote next to a task's transcripts. Every field optional:
/// these files predate the schema.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskMeta {
    pub task_id: i64,
    pub cwd: String,
    pub session_id: String,
    pub session_file: String,
    pub archived_at: String,
}

/// The archived transcript itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptInfo {
    pub path: String,
    pub bytes: u64,
    pub mtime: String,
}

/// A saved spawn prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInfo {
    pub path: String,
    pub at: String,
    pub bytes: u64,
}

/// One daily-log line, as the viewer shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogInfo {
    pub date: String,
    pub title: String,
    pub summary: String,
    pub mr_url: Option<String>,
    pub time: Option<String>,
}

/// Everything known about one archived task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArchiveView {
    pub task_id: i64,
    pub cwd: String,
    pub repo: String,
    pub session_id: Option<String>,
    pub session_file: Option<String>,
    pub archived_at: Option<String>,
    pub transcript: Option<TranscriptInfo>,
    pub prompts: Vec<PromptInfo>,
    pub logs: Vec<LogInfo>,
    /// Journal entries that reference this task; filled by
    /// [`super::collect`].
    pub entry_ids: Vec<String>,
}

impl TaskArchiveView {
    /// The command the viewer offers for copy-paste. `None` without both a
    /// directory and a session id — a half-written one would not work.
    pub fn resume_command(&self) -> Option<String> {
        let session_id = self.session_id.as_deref()?;
        (!self.cwd.is_empty()).then(|| format!("cd {} && claude --resume {session_id}", self.cwd))
    }
}

/// Every `tasks/<id>/meta.json` that parses. An archive without one is skipped
/// rather than invented.
pub fn list_task_metas(paths: &Paths) -> Vec<TaskMeta> {
    let Ok(entries) = fs::read_dir(&paths.tasks_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(dir_task_id) = name.parse::<i64>() else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(entry.path().join("meta.json")) else {
            continue;
        };
        let Ok(mut meta) = serde_json::from_str::<TaskMeta>(&raw) else {
            continue;
        };
        if meta.task_id == 0 {
            meta.task_id = dir_task_id;
        }
        out.push(meta);
    }
    out.sort_by_key(|meta| meta.task_id);
    out
}

fn transcript_info(paths: &Paths, task_id: i64, session_id: &str) -> Option<TranscriptInfo> {
    if session_id.is_empty() {
        return None;
    }
    let path = paths.task_dir(task_id).join(format!("{session_id}.jsonl"));
    let meta = fs::metadata(&path).ok()?;
    Some(TranscriptInfo {
        path: path.display().to_string(),
        bytes: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .map(iso_from_system_time)
            .unwrap_or_default(),
    })
}

fn iso_from_system_time(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `task-<id>-<epoch millis>.txt` in the prompts directory, oldest first.
fn list_prompts(paths: &Paths) -> BTreeMap<i64, Vec<PromptInfo>> {
    let mut by_task: BTreeMap<i64, Vec<PromptInfo>> = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&paths.prompts_dir) else {
        return by_task;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((task_id, stamp)) = parse_prompt_name(&name) else {
            continue;
        };
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        by_task.entry(task_id).or_default().push(PromptInfo {
            path: entry.path().display().to_string(),
            at: iso_from_millis(stamp),
            bytes,
        });
    }
    for list in by_task.values_mut() {
        list.sort_by(|a, b| a.at.cmp(&b.at));
    }
    by_task
}

/// `task-5944-1756800000000.txt` → `(5944, 1756800000000)`.
fn parse_prompt_name(name: &str) -> Option<(i64, i64)> {
    let rest = name.strip_prefix("task-")?.strip_suffix(".txt")?;
    let (task, stamp) = rest.split_once('-')?;
    if task.is_empty() || stamp.is_empty() {
        return None;
    }
    Some((task.parse().ok()?, stamp.parse().ok()?))
}

fn iso_from_millis(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// Daily-log lines grouped by task, oldest day first.
fn list_log_entries(paths: &Paths, db: Option<&Db>) -> BTreeMap<i64, Vec<LogInfo>> {
    let mut by_task: BTreeMap<i64, Vec<LogInfo>> = BTreeMap::new();
    for entry in dailylog::read_all_entries(paths, db) {
        by_task
            .entry(entry.line.task_id)
            .or_default()
            .push(LogInfo {
                date: entry.day,
                title: entry.line.title,
                summary: entry.line.short,
                mr_url: entry.line.mr_url,
                time: Some(entry.line.time).filter(|t| !t.is_empty()),
            });
    }
    by_task
}

/// Everything the viewer's Task-archive tab lists, newest task first.
pub fn load_tasks(paths: &Paths, db: Option<&Db>) -> Vec<TaskArchiveView> {
    let mut tasks: BTreeMap<i64, TaskArchiveView> = BTreeMap::new();
    let ensure = |tasks: &mut BTreeMap<i64, TaskArchiveView>, task_id: i64| {
        tasks.entry(task_id).or_insert_with(|| TaskArchiveView {
            task_id,
            ..TaskArchiveView::default()
        });
    };

    for meta in list_task_metas(paths) {
        ensure(&mut tasks, meta.task_id);
        let task = tasks.get_mut(&meta.task_id).expect("just inserted");
        task.repo = if meta.cwd.is_empty() {
            String::new()
        } else {
            base_name(Path::new(&meta.cwd))
        };
        task.transcript = transcript_info(paths, meta.task_id, &meta.session_id);
        task.session_id = Some(meta.session_id).filter(|s| !s.is_empty());
        task.session_file = Some(meta.session_file).filter(|s| !s.is_empty());
        task.archived_at = Some(meta.archived_at).filter(|s| !s.is_empty());
        task.cwd = meta.cwd;
    }
    for (task_id, prompts) in list_prompts(paths) {
        ensure(&mut tasks, task_id);
        tasks.get_mut(&task_id).expect("just inserted").prompts = prompts;
    }
    for (task_id, logs) in list_log_entries(paths, db) {
        ensure(&mut tasks, task_id);
        tasks.get_mut(&task_id).expect("just inserted").logs = logs;
    }

    let mut out: Vec<TaskArchiveView> = tasks.into_values().collect();
    out.sort_by_key(|task| std::cmp::Reverse(task.task_id));
    out
}
