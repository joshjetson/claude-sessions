//! The pipeline that actually runs for a project: the built-in definition with
//! the project's overrides merged over it, and the prompt assembled from it.
//!
//! A project customises its pipeline by committing a file to its own repo:
//!
//! ```text
//! <repo>/.claude-sessions/pipeline.json
//! ```
//!
//! Living in the repo (rather than in `~/.claude-sessions.json`) means the
//! pipeline is version-controlled with the code it builds, and every teammate
//! gets the same one:
//!
//! ```json
//! {
//!   "extends": "task",
//!   "vars": { "targetBranch": "main" },
//!   "steps": {
//!     "finish":     { "run": "./scripts/post-merge.sh" },
//!     "journal":    { "skip": true },
//!     "smoke-test": { "after": "open-mr", "title": "Smoke test", "prompt": "..." }
//!   }
//! }
//! ```
//!
//! Per step an override may: `skip` it (still drawn, marked skipped), relabel it
//! (`title`/`what`/`detail`), point it at another `skill`, replace its `prompt`
//! outright, `append` a sentence, name a command to `run`, set the `stage` a
//! stage-move step targets, or position a NEW step with `after`/`before`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::definitions::{built_in, Fragment, PipelineDef, StepDef, StepKind};
use super::project::{
    apply_patch, has_real_keys, patch_bool, patch_kind, patch_str, read_project_override,
    PipelineSource,
};
use super::vars::{interpolate, PromptVars};

/// How a step's prompt text is produced, after any override.
#[derive(Clone)]
pub(super) struct StepPrompt {
    pub(super) base: PromptBase,
    pub(super) run: Option<String>,
    pub(super) append: Option<String>,
}

#[derive(Clone)]
pub(super) enum PromptBase {
    /// The step's built-in fragment.
    Builtin(Fragment),
    /// `prompt` in an override — replaces the fragment entirely.
    Replaced(String),
}

/// One step of a resolved pipeline, carrying what a project changed so the
/// viewers can show it.
#[derive(Clone)]
pub struct ResolvedStep {
    pub id: String,
    pub title: String,
    pub skill: Option<String>,
    pub what: String,
    pub detail: String,
    pub gate: bool,
    pub kind: StepKind,
    pub stage: Option<String>,
    pub customised: bool,
    pub skipped: bool,
    pub added: bool,
    pub(super) prompt: StepPrompt,
}

impl ResolvedStep {
    fn from_def(def: &StepDef) -> Self {
        ResolvedStep {
            id: def.id.to_string(),
            title: def.title.to_string(),
            skill: def.skill.map(str::to_string),
            what: def.what.to_string(),
            detail: def.detail.to_string(),
            gate: def.gate,
            kind: def.kind,
            stage: def.stage.map(str::to_string),
            customised: false,
            skipped: false,
            added: false,
            prompt: StepPrompt {
                base: PromptBase::Builtin(def.fragment),
                run: None,
                append: None,
            },
        }
    }

    /// The text this step contributes to the spawn prompt. Empty means the step
    /// is drawn in the diagram but says nothing to the agent.
    pub fn render(&self, vars: &PromptVars, project_vars: &BTreeMap<String, String>) -> String {
        let mut text = match &self.prompt.base {
            PromptBase::Builtin(fragment) => fragment(vars),
            PromptBase::Replaced(text) => {
                format!(" {}", interpolate(text, vars, project_vars).trim())
            }
        };
        if let Some(run) = &self.prompt.run {
            // A command to run at this step, appended to whatever the step
            // already says — `run` adds, it never replaces.
            text = format!(
                "{text} Then run this project's command for this step: `{}` (from the repository root) and report its outcome.",
                interpolate(run, vars, project_vars)
            );
        }
        if let Some(append) = &self.prompt.append {
            text = format!("{text} {}", interpolate(append, vars, project_vars).trim());
        }
        text
    }
}

impl fmt::Debug for ResolvedStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedStep")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("customised", &self.customised)
            .field("skipped", &self.skipped)
            .field("added", &self.added)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedPipeline {
    pub id: String,
    pub name: String,
    pub trigger: String,
    pub summary: String,
    pub preamble: String,
    pub steps: Vec<ResolvedStep>,
    /// The project's `vars`, used as defaults during interpolation.
    pub vars: BTreeMap<String, String>,
    pub source: PipelineSource,
    pub override_file: Option<PathBuf>,
    pub override_error: Option<String>,
}

impl ResolvedPipeline {
    /// The steps that will actually run, in order.
    pub fn active_steps(&self) -> Vec<&ResolvedStep> {
        self.steps.iter().filter(|step| !step.skipped).collect()
    }

    pub fn step(&self, id: &str) -> Option<&ResolvedStep> {
        self.steps.iter().find(|step| step.id == id)
    }

    /// One step's contribution, with this project's vars applied.
    pub fn prompt_for(&self, step: &ResolvedStep, vars: &PromptVars) -> String {
        step.render(vars, &self.vars)
    }

    /// Assemble the spawn prompt: the preamble, then every active step's
    /// fragment in order.
    pub fn build_prompt(&self, vars: &PromptVars) -> String {
        let mut out = self.preamble.clone();
        for step in self.steps.iter().filter(|step| !step.skipped) {
            out.push_str(&step.render(vars, &self.vars));
        }
        out
    }
}

/// Asking for a pipeline that does not exist. Worth an error rather than an
/// empty prompt: a session spawned with no instructions looks like it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPipeline(pub String);

impl fmt::Display for UnknownPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unknown pipeline \"{}\"", self.0)
    }
}

impl std::error::Error for UnknownPipeline {}

/// Merge a project override onto a built-in pipeline.
pub fn resolve_pipeline(
    pipeline_id: &str,
    repo_path: Option<&Path>,
) -> Result<ResolvedPipeline, UnknownPipeline> {
    let base: &PipelineDef =
        built_in(pipeline_id).ok_or_else(|| UnknownPipeline(pipeline_id.to_string()))?;
    let over = read_project_override(repo_path);

    let mut resolved = ResolvedPipeline {
        id: base.id.to_string(),
        name: base.name.to_string(),
        trigger: base.trigger.to_string(),
        summary: base.summary.to_string(),
        preamble: base.preamble.to_string(),
        steps: base.steps.iter().map(ResolvedStep::from_def).collect(),
        vars: BTreeMap::new(),
        source: PipelineSource::BuiltIn,
        override_file: over.as_ref().map(|over| over.file.clone()),
        override_error: over.as_ref().and_then(|over| over.error.clone()),
    };

    let Some(over) = over else {
        return Ok(resolved);
    };
    if over.error.is_some() {
        return Ok(resolved);
    }
    // An override for a different pipeline doesn't apply to this one.
    if over.extends.as_deref().is_some_and(|id| id != pipeline_id) {
        return Ok(resolved);
    }

    // A patch made only of `$` hints is not a change: the generated template
    // lists every step with its $what/$edit notes, and marking those as
    // customised would light up the whole flow before you edited a thing.
    let touched = over
        .steps
        .values()
        .any(|patch| patch.as_object().is_some_and(has_real_keys));
    resolved.source = if touched {
        PipelineSource::Project
    } else {
        PipelineSource::BuiltIn
    };
    resolved.vars = over.vars.clone();

    // Existing steps first…
    for (id, patch) in &over.steps {
        let Some(patch) = patch.as_object() else {
            continue;
        };
        let Some(index) = resolved.steps.iter().position(|step| &step.id == id) else {
            continue;
        };
        apply_patch(&mut resolved.steps[index], patch);
        if has_real_keys(patch) {
            resolved.steps[index].customised = true;
        }
    }

    // …then new ones, positioned relative to an existing step.
    for (id, patch) in &over.steps {
        let Some(patch) = patch.as_object() else {
            continue;
        };
        if resolved.steps.iter().any(|step| &step.id == id) {
            continue;
        }
        let mut step = ResolvedStep {
            id: id.clone(),
            title: patch_str(patch, "title").unwrap_or_else(|| id.clone()),
            skill: patch_str(patch, "skill"),
            what: patch_str(patch, "what").unwrap_or_else(|| "Added by this project.".to_string()),
            detail: patch_str(patch, "detail").unwrap_or_default(),
            gate: patch_bool(patch, "gate"),
            kind: patch_kind(patch).unwrap_or(StepKind::Agent),
            stage: patch_str(patch, "stage"),
            customised: true,
            added: true,
            skipped: patch_bool(patch, "skip"),
            prompt: StepPrompt {
                base: PromptBase::Builtin(super::definitions::no_prompt),
                run: None,
                append: None,
            },
        };
        apply_patch(&mut step, patch);

        // No anchor means last — after `finish`, which surprises people often
        // enough that the starter template warns about it.
        let after = patch_str(patch, "after").filter(|id| !id.is_empty());
        let before = patch_str(patch, "before").filter(|id| !id.is_empty());
        let anchor = after.clone().or(before);
        let index = anchor
            .and_then(|anchor| resolved.steps.iter().position(|step| step.id == anchor))
            .map(|at| if after.is_some() { at + 1 } else { at })
            .unwrap_or(resolved.steps.len());
        resolved.steps.insert(index, step);
    }

    Ok(resolved)
}

/// What the dashboard should do about a stage-move step.
#[derive(Debug, Clone)]
pub struct DashboardStep {
    pub enabled: bool,
    pub stage: Option<String>,
    pub step: Option<ResolvedStep>,
}

/// Look up a dashboard step (a stage move) for a project.
///
/// The dashboard consults this before moving a task, so the pipeline you can
/// see is the one that runs: skip the step and the move stops happening; set
/// `stage` and it moves somewhere else. Everything that can go wrong — an
/// unknown pipeline, an unreadable override, a step the project deleted —
/// degrades to "do the default thing". Failing closed here would silently
/// strand every finished task in progress.
pub fn dashboard_step(pipeline_id: &str, repo_path: Option<&Path>, step_id: &str) -> DashboardStep {
    let default = DashboardStep {
        enabled: true,
        stage: None,
        step: None,
    };
    let Ok(resolved) = resolve_pipeline(pipeline_id, repo_path) else {
        return default;
    };
    match resolved.step(step_id) {
        Some(step) => DashboardStep {
            enabled: !step.skipped,
            stage: step.stage.clone(),
            step: Some(step.clone()),
        },
        None => default,
    }
}
