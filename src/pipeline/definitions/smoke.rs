//! A smoke test: a fast check that each acceptance criterion works on its main
//! path. QAden's `/smoke-test <N>` does the work.
//!
//! It sits next to QA on the task menu, and it is not QA. Three things differ,
//! each on purpose:
//!
//! * There is no verdict, no round and no `run.json`. The skill ends with a
//!   smoke note in the terminal, so nothing is parked and nothing waits for the
//!   reviewer's approval.
//! * There is no reviewer token. Nothing a reviewer says is relayed to a smoke
//!   test, so it holds nothing to sign with.
//! * It does not count as the task's QA session. The launch exports
//!   `CLAUDE_SESSIONS_LAUNCH_KIND=smoke`, and the QA guards, QA runs and
//!   reviewer answers skip a session that carries it. A second smoke test on
//!   the same task is still refused unless forced.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::PromptVars;

pub static SMOKE_PIPELINE: PipelineDef = PipelineDef {
    id: "smoke",
    name: "Smoke test a task",
    trigger: "Enter on a board task → Smoke test",
    summary: "Runs QAden's /smoke-test: one main-path check per acceptance criterion. Gives no verdict, keeps no run.json, posts nothing to Odoo.",
    preamble: "You are smoke testing this task: a fast check that each acceptance criterion works on its main path. This is not a QA pass. It gives no verdict, and it does not replace one.",
    steps: &[
        StepDef::new("context", "Task context", "Hands over the task URL and any extra context typed at launch.", context)
            .detail("Same reason as the QA pipelines: sessions link to tasks by the Odoo task URL in the transcript, not by the environment variable. Without the URL the Sessions tab shows no task number for the session."),
        StepDef::new("smoke", "Smoke test", "Runs /smoke-test: each acceptance criterion once, on its main path.", smoke)
            .skill("smoke-test")
            .gate()
            .detail("The skill stops with three choices when a criterion fails, so the run can stop here for a person."),
        StepDef::new("report-only", "Report in the terminal", "Prints the smoke note and stops. Posts nothing, flags nothing.", report_only)
            .detail("No verdict is parked and the dashboard is not notified: a smoke note is not a QA hand-back, and a verdict-shaped notification would invite an approval nothing is waiting for."),
    ],
};

fn context(vars: &PromptVars) -> String {
    format!(
        " The task is {}.{}",
        vars.url,
        vars.extra_context_sentence()
    )
}

fn smoke(vars: &PromptVars) -> String {
    format!(
        " Run /smoke-test {} and follow that skill exactly.",
        vars.task_id
    )
}

fn report_only(_vars: &PromptVars) -> String {
    " Do NOT post anything to Odoo, do NOT move the task to another stage, do NOT tag anyone, and do NOT write to the QA tab. \
Write no run.json, no pass note and no revision note, and do not notify the dashboard. \
Print the smoke note in the terminal, unfenced, then stop and wait for the user. Do not end the session."
        .to_string()
}
