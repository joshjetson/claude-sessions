//! The pipelines: what the dashboard tells a spawned agent to do.
//!
//! [`definitions`] holds the six built-in pipelines as data; [`resolve`] merges
//! a project's `pipeline.json` over one and assembles the spawn prompt;
//! [`skills`] finds the skills a step can name; [`template`] writes the starter
//! override and locates a step inside it.
//!
//! The assembled prompt is pinned by golden-master tests. Every step's wording
//! is load-bearing — agents act on it — so a stray space is a behaviour change,
//! not a formatting nit.

pub mod definitions;
pub mod html;
pub mod project;
pub mod resolve;
pub mod skills;
pub mod template;
mod vars;

pub use definitions::{
    built_in, pipeline_skills, PipelineDef, StepDef, StepKind, BUILT_IN, CONFLICT_PIPELINE,
    PRE_OPTICS_PIPELINE, QA_DRYRUN_PIPELINE, QA_PIPELINE, REVISION_PIPELINE, TASK_PIPELINE,
};
pub use html::{render_html, sample_vars, ProjectRepo};
pub use project::{read_project_override, PipelineSource, ProjectOverride};
pub use resolve::{
    dashboard_step, resolve_pipeline, DashboardStep, ResolvedPipeline, ResolvedStep,
    UnknownPipeline,
};
pub use skills::{discover_skills, parse_skill_frontmatter, Skill};
pub use template::{init_project_pipeline, starter_template, step_line_number, InitError};
pub use vars::{
    bin, branch_instruction, interpolate, BranchFallback, MergeRequestVars, PromptVars, BIN_NAME,
};

#[cfg(test)]
mod tests;
