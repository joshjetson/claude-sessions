//! The pipelines, as data.
//!
//! These were template literals inside the Node app's `tui/actions.js`, which
//! meant the only way to know what the dashboard tells an agent to do was to
//! read the source. Here each step carries BOTH the prompt fragment it
//! contributes and a description of what it does — so the diagram, the docs and
//! the prompt that actually runs come from one definition and cannot drift.
//!
//! A step's fields:
//!
//! * `id` — stable key; what a project override patches.
//! * `title` — short label for the diagram.
//! * `skill` — the Claude Code skill it invokes, if any. A test asserts the
//!   fragment really names it.
//! * `what` / `detail` — one line, then the longer explanation for the viewer.
//! * `gate` — the run can stop here.
//! * `kind` — [`StepKind::Agent`] contributes prompt text; [`StepKind::Dashboard`]
//!   is something the tool itself does. Stage moves are dashboard steps: the
//!   agent is never told to move the task.
//! * `stage` — for a stage-move step, the stage name to prefer. `None` falls
//!   back to the configured and known names in [`crate::odoo`].
//! * `fragment` — the prompt text this step contributes.
//!
//! A fragment returning an empty string drops the step from the prompt while
//! keeping it in the diagram.

mod conflict;
mod pre_optics;
mod qa;
mod revision;
mod task;

pub use conflict::CONFLICT_PIPELINE;
pub use pre_optics::PRE_OPTICS_PIPELINE;
pub use qa::{QA_DRYRUN_PIPELINE, QA_PIPELINE};
pub use revision::REVISION_PIPELINE;
pub use task::TASK_PIPELINE;

use super::PromptVars;

/// The prompt text a step contributes, built from the run's variables.
pub type Fragment = fn(&PromptVars) -> String;

/// A step the agent never sees. Dashboard steps use this.
pub fn no_prompt(_vars: &PromptVars) -> String {
    String::new()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StepKind {
    /// An instruction in the spawn prompt.
    #[default]
    Agent,
    /// Something the dashboard does — a stage move — contributing no text.
    Dashboard,
}

#[derive(Debug, Clone, Copy)]
pub struct StepDef {
    pub id: &'static str,
    pub title: &'static str,
    pub skill: Option<&'static str>,
    pub what: &'static str,
    pub detail: &'static str,
    pub gate: bool,
    pub kind: StepKind,
    pub stage: Option<&'static str>,
    pub fragment: Fragment,
}

impl StepDef {
    pub const fn new(
        id: &'static str,
        title: &'static str,
        what: &'static str,
        fragment: Fragment,
    ) -> Self {
        StepDef {
            id,
            title,
            skill: None,
            what,
            detail: "",
            gate: false,
            kind: StepKind::Agent,
            stage: None,
            fragment,
        }
    }

    /// A stage move: no prompt text, and a stage the project may redirect.
    pub const fn dashboard(id: &'static str, title: &'static str, what: &'static str) -> Self {
        let mut step = StepDef::new(id, title, what, no_prompt);
        step.kind = StepKind::Dashboard;
        step
    }

    pub const fn skill(mut self, skill: &'static str) -> Self {
        self.skill = Some(skill);
        self
    }

    pub const fn detail(mut self, detail: &'static str) -> Self {
        self.detail = detail;
        self
    }

    /// The run can stop here — a verdict, a gate, a checkpoint for a human.
    pub const fn gate(mut self) -> Self {
        self.gate = true;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PipelineDef {
    pub id: &'static str,
    pub name: &'static str,
    /// The key or menu entry that starts it, for the viewer.
    pub trigger: &'static str,
    pub summary: &'static str,
    /// The opening sentence, before any step contributes.
    pub preamble: &'static str,
    pub steps: &'static [StepDef],
}

/// Every pipeline the dashboard can run, in menu order.
pub static BUILT_IN: &[&PipelineDef] = &[
    &TASK_PIPELINE,
    &REVISION_PIPELINE,
    &QA_PIPELINE,
    &QA_DRYRUN_PIPELINE,
    &PRE_OPTICS_PIPELINE,
    &CONFLICT_PIPELINE,
];

pub fn built_in(id: &str) -> Option<&'static PipelineDef> {
    BUILT_IN.iter().copied().find(|pipeline| pipeline.id == id)
}

/// The skills the built-in pipelines invoke, sorted and deduplicated.
///
/// Derived from the definitions rather than kept as a second list: the Node
/// version hand-maintained one and it had already drifted — it named two skills
/// no pipeline invoked.
pub fn pipeline_skills() -> Vec<&'static str> {
    let mut skills: Vec<&'static str> = BUILT_IN
        .iter()
        .flat_map(|pipeline| pipeline.steps.iter())
        .filter_map(|step| step.skill)
        .collect();
    skills.sort_unstable();
    skills.dedup();
    skills
}
