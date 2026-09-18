//! The other end of a task's life. `/qa` judges delivered work against
//! criteria; this runs when there is no branch and nothing has been built, and
//! asks what is actually being requested.
//!
//! Two differences from the QA pipelines, both following from that position:
//!
//! * There is no verdict and nothing to park. The output is a brief plus an
//!   Optics Process, and the person who reads it is the developer about to
//!   start. So the run ends by printing the brief, not by forking on pass/fail.
//! * It belongs on a task in Backlog or Approved to Start, not in QA. Nothing
//!   enforces that — the menu offers it on any row — because a task that has
//!   gone backwards, or one whose scope turned out wrong mid-build, is exactly
//!   when a brief is worth writing and a stage check would refuse it.
//!
//! Optics support exists only for the projects configured for it. The skill
//! stops and says so rather than producing half an artifact, so this does not
//! duplicate the check.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::{bin, PromptVars};

pub static PRE_OPTICS_PIPELINE: PipelineDef = PipelineDef {
    id: "pre-optics",
    name: "Pre-work brief (Optics)",
    trigger: "Enter on a board task → Pre-work brief",
    summary: "Investigates a task before any work exists and hands the developer an Optics Process plus a written brief. Posts nothing to Odoo.",
    preamble: "You are writing a pre-work brief for a task nobody has started yet. Your job is to work out what is actually being asked and what the task does not say — not to design the fix. A confident brief that is wrong costs a developer a day, so say plainly where you are uncertain rather than smoothing it over.",
    steps: &[
        StepDef::new("context", "Task context", "Hands over the task URL and any extra context typed at launch.", context)
            .detail("Same reason as the QA pipelines: sessions link to tasks by the Odoo task URL appearing in the transcript, not by the environment variable."),
        StepDef::new("pre-optics", "Investigate and brief", "Runs /pre-optics: works out what is being asked, records an Optics Process, writes the brief.", investigate)
            .skill("pre-optics")
            .gate()
            .detail("The skill runs its own clarifying-question round and stops for answers. That pause is the point of it — the failure mode it exists to prevent is a confident, wrong brief that sends a developer a day in the wrong direction."),
        StepDef::new("hand-over", "Hand over the brief", "Prints the brief and notifies the board. Posts nothing.", hand_over)
            .detail("Same rule as QA: the agent never writes to Odoo. A brief carries assumptions someone has to agree with before a developer builds on them, so a person decides whether it is posted."),
    ],
};

fn context(vars: &PromptVars) -> String {
    format!(
        " The task is {}.{}",
        vars.url,
        vars.extra_context_sentence()
    )
}

fn investigate(vars: &PromptVars) -> String {
    format!(
        " Run /pre-optics {} and follow that skill exactly, including its clarifying-question round.",
        vars.task_id
    )
}

fn hand_over(vars: &PromptVars) -> String {
    let task_id = vars.task_id;
    format!(
        " Do NOT post anything to Odoo, do NOT move the task to another stage, and do NOT tag anyone. \
Then flag the brief for review by running: {} notify --title \"Pre-work brief #{task_id} ready\" --message \"<one line on what the task actually asks, then the Optics process name and the path to the brief>\" --level info. \
Finally print the brief in the terminal, unfenced, then stop and wait for the user. Do not end the session.",
        bin()
    )
}
