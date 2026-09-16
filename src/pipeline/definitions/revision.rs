//! "Handle a revision" — resumes the original session with its full context and
//! addresses QA feedback on the same branch and MR.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::{done_command, PromptVars};

pub static REVISION_PIPELINE: PipelineDef = PipelineDef {
    id: "revision",
    name: "Handle a revision",
    trigger: "v on a board task",
    summary: "Resumes the original session with its full context and addresses QA feedback on the same branch and MR.",
    preamble: "A revision was requested on this task. You have full prior context from this resumed session — your earlier decisions, reasoning, files investigated, and changes made.",
    steps: &[
        StepDef::new("context", "Resumed context", "Re-enters the session that did the original work.", context)
            .detail("Because the session is resumed rather than started fresh, the agent still has its earlier decisions, reasoning and file exploration. That is why this pipeline is much shorter than the task one — it does not need to rebuild understanding."),
        StepDef::dashboard("move-in-progress", "Move to In Progress", "The dashboard moves the task into the project's working stage.")
            .detail("Resolution order: the `stage` set here, then config.inProgressStage, then known names (\"In Progress\", \"Doing\", \"In Development\", \"WIP\"…), then a fuzzy match. If the project has no working-like stage the task is left where it is. Best-effort: an Odoo failure is reported but never blocks the session from launching."),
        StepDef::new("revise", "Address the feedback", "Parses the QA notes and makes narrowly-scoped fixes.", revise)
            .skill("handle-revision")
            .detail("Deliberately scoped to the revision items only — it does not re-open the whole task."),
        StepDef::new("push", "Push to the same MR", "Pushes fixes to the existing branch so the open MR picks them up.", push)
            .detail("Opens an MR only if one somehow does not exist yet."),
        StepDef::new("summary", "Write the summary", "Same three-section write-up, describing the QA issue.", summary)
            .detail("Root cause here means what QA actually caught, not the original ticket."),
        StepDef::new("journal", "Record the lesson", "A revision means the first attempt missed something — capture why.", journal)
            .skill("problem-reasoning-journal")
            .detail("Weighted more strongly than in the task pipeline: QA catching something is itself the signal that a reusable lesson exists."),
        StepDef::dashboard("move-qa", "Move to QA", "The dashboard moves the finished task into the project's QA stage.")
            .detail("Runs after the agent signals completion. Resolution order: the `stage` set here, then config.doneStage, then known QA names (\"Quality Assurance\", \"QA\", \"Ready for QA\", \"Testing\", \"UAT\"…), then a fuzzy QA match. If nothing matches, the task is deliberately left alone rather than advanced — guessing could land it in \"Revision Required\"."),
        StepDef::new("finish", "Finish", "Signals completion so the dashboard can update Odoo.", finish)
            .detail("Same completion path as the task pipeline."),
    ],
};

fn context(vars: &PromptVars) -> String {
    vars.extra_context_sentence()
}

fn revise(vars: &PromptVars) -> String {
    format!(
        " Run /handle-revision for {} to address the QA feedback.",
        vars.url
    )
}

fn push(vars: &PromptVars) -> String {
    format!(
        " Commit and push your fixes, and make sure the existing merge request reflects them (push to the same branch; if no MR exists yet, open one with glab targeting {}).",
        vars.branch_instruction
    )
}

fn summary(vars: &PromptVars) -> String {
    format!(
        " Once the revision is pushed and the MR is up to date, write a short plain-English summary to {} using EXACTLY these three markdown sections — \"**Root cause:**\" (what the QA issue actually was, in plain language), \"**Fix:**\" (what you changed and why), and \"**Before vs after:**\" (the previous behavior/output versus the new behavior/output). Keep it concise.",
        vars.summary_file
    )
}

fn journal(_vars: &PromptVars) -> String {
    " A revision means the first attempt missed something — if there's a reusable lesson (a wrong assumption, a hidden business rule, or why QA caught this), run /problem-reasoning-journal to record it so it isn't repeated; skip it only for trivial fixes.".to_string()
}

fn finish(vars: &PromptVars) -> String {
    format!(
        " Then run: {}",
        done_command(vars.task_id, &vars.summary_file)
    )
}
