//! The project override file — `<repo>/.claude-sessions/pipeline.json` — and
//! the rules for applying one step patch.
//!
//! Split out of [`super::resolve`] so the merge logic reads as merge logic: this
//! file is only about what the file on disk says and what each key means.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::definitions::StepKind;
use super::resolve::{PromptBase, ResolvedStep};

pub const PROJECT_DIR: &str = ".claude-sessions";
pub const PROJECT_FILE: &str = "pipeline.json";

pub fn project_pipeline_path(repo_path: &Path) -> PathBuf {
    repo_path.join(PROJECT_DIR).join(PROJECT_FILE)
}

/// Whether what you are looking at is the shipped pipeline or one a project
/// changed. Shown in the viewers, so an untouched template must not claim to be
/// a customisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineSource {
    BuiltIn,
    Project,
}

/// A project's `pipeline.json`, as read off disk.
#[derive(Debug, Clone, Default)]
pub struct ProjectOverride {
    pub file: PathBuf,
    /// Why it could not be used. A broken file is reported, never fatal.
    pub error: Option<String>,
    pub extends: Option<String>,
    pub vars: BTreeMap<String, String>,
    pub(super) steps: Map<String, Value>,
}

/// Read `<repo>/.claude-sessions/pipeline.json`. `None` means there is none.
pub fn read_project_override(repo_path: Option<&Path>) -> Option<ProjectOverride> {
    let file = project_pipeline_path(repo_path?);
    let text = fs::read_to_string(&file).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Some(ProjectOverride {
            extends: value
                .get("extends")
                .and_then(Value::as_str)
                .map(str::to_string),
            // `$`-prefixed keys are documentation in the generated template and
            // never overrides.
            vars: value
                .get("vars")
                .and_then(Value::as_object)
                .map(|vars| {
                    vars.iter()
                        .filter(|(key, _)| !key.starts_with('$'))
                        .map(|(key, value)| (key.clone(), scalar_to_string(value)))
                        .collect()
                })
                .unwrap_or_default(),
            steps: value
                .get("steps")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            error: None,
            file,
        }),
        Err(err) => Some(ProjectOverride {
            file,
            error: Some(err.to_string()),
            ..ProjectOverride::default()
        }),
    }
}

pub(super) fn has_real_keys(patch: &Map<String, Value>) -> bool {
    patch.keys().any(|key| !key.starts_with('$'))
}

pub(super) fn apply_patch(step: &mut ResolvedStep, patch: &Map<String, Value>) {
    if patch.contains_key("stage") {
        step.stage = patch_str(patch, "stage");
    }
    if let Some(kind) = patch_kind(patch) {
        step.kind = kind;
    }
    if let Some(title) = patch_str(patch, "title") {
        step.title = title;
    }
    if let Some(what) = patch_str(patch, "what") {
        step.what = what;
    }
    if let Some(detail) = patch_str(patch, "detail") {
        step.detail = detail;
    }
    if patch.contains_key("skill") {
        step.skill = patch_str(patch, "skill");
    }
    if patch.contains_key("gate") {
        step.gate = patch_bool(patch, "gate");
    }
    if patch_bool(patch, "skip") {
        step.skipped = true;
    }

    if let Some(prompt) = patch_str(patch, "prompt") {
        step.prompt.base = PromptBase::Replaced(prompt);
    }
    step.prompt.run = patch_str(patch, "run").or(step.prompt.run.take());
    step.prompt.append = patch_str(patch, "append").or(step.prompt.append.take());
}

pub(super) fn patch_str(patch: &Map<String, Value>, key: &str) -> Option<String> {
    match patch.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(scalar_to_string(other)),
    }
}

pub(super) fn patch_bool(patch: &Map<String, Value>, key: &str) -> bool {
    match patch.get(key) {
        Some(Value::Bool(value)) => *value,
        // JavaScript truthiness, which is what the file was written against.
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        _ => false,
    }
}

pub(super) fn patch_kind(patch: &Map<String, Value>) -> Option<StepKind> {
    match patch.get("kind")?.as_str()? {
        "dashboard" => Some(StepKind::Dashboard),
        _ => Some(StepKind::Agent),
    }
}

/// Values are interpolated into prompt text, so a number or a bool reads as it
/// would have been written.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
