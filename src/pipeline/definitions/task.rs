//! "Start a task" — from an unread ticket to an open merge request, gating
//! first on whether the ticket is understood well enough to code.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::{bin, blocked_command, done_command, notify_command, PromptVars};

pub static TASK_PIPELINE: PipelineDef = PipelineDef {
    id: "task",
    name: "Start a task",
    trigger: "s on a board task",
    summary: "Takes an Odoo task from unread to an open merge request, gating first on whether it is understood well enough to code.",
    preamble: "Work this Odoo task end-to-end.",
    steps: &[
        StepDef::new("context", "Prior context", "Loads anything already known about this task before work starts.", context)
            .detail("Extra context you typed is passed through verbatim and told to outrank generic assumptions. If the task was worked before, the archived transcript of that session is offered so the agent does not redo finished work."),
        StepDef::dashboard("move-in-progress", "Move to In Progress", "The dashboard moves the task into the project's working stage.")
            .detail("Resolution order: the `stage` set here, then config.inProgressStage, then known names (\"In Progress\", \"Doing\", \"In Development\", \"WIP\"…), then a fuzzy match. If the project has no working-like stage the task is left where it is. Best-effort: an Odoo failure is reported but never blocks the session from launching."),
        StepDef::new("readiness-gate", "Readiness gate", "Decides whether the ticket is understood well enough to safely write code.", readiness_gate)
            .skill("task-readiness-gate")
            .gate()
            .detail("Loads the task, its comments and attachments, explores the codebase, then returns one of four verdicts. BLOCKED_NEEDS_INFO stops the run: the agent posts clarifying questions to the task, flags it on the dashboard, and touches no code. READY_FOR_REPRO means reproduce before fixing; READY_BUT_NO_REPRO means post your assumptions and proceed carefully."),
        StepDef::new("review", "Review & plan", "Explores the code and posts an implementation plan to the task.", review)
            .skill("odoo-review")
            .detail("Reads the ticket, finds the code it touches, and writes a plan into the Odoo task so the approach is on record before anything is written."),
        StepDef::new("implement", "Implement", "Creates the branch and writes the code against the posted plan.", implement)
            .skill("begin-odoo-task")
            .detail("Must run all the way through its final step. Stopping after a push is treated as incomplete."),
        StepDef::new("open-mr", "Commit, push, open MR", "Opens a merge request with glab against the project's target branch.", open_mr)
            .detail("The most common failure mode is an agent that pushes and stops, leaving no MR. The prompt calls that out explicitly and requires the MR URL to be verified before finishing."),
        StepDef::new("escalate", "Escalate if blocked", "Notifies the dashboard and waits, rather than guessing.", escalate)
            .detail("Any point where the agent needs a decision or missing context it cannot resolve. The notification reaches the dashboard feed with a sound."),
        StepDef::new("summary", "Write the summary", "Produces the plain-English write-up that lands on the task and in the standup log.", summary)
            .detail("Three fixed sections — root cause, fix, before vs after — written for a non-engineer. The dashboard converts it to HTML for the Odoo chatter and condenses it to one line for the daily log."),
        StepDef::new("journal", "Record the reasoning", "Captures how the solution was found, when the work was non-trivial.", journal)
            .skill("problem-reasoning-journal")
            .detail("Runs only for real investigations — debugging across systems, a wrong first assumption, a hidden business rule. Skipped for one-line changes. Feeds the journal viewer."),
        StepDef::dashboard("move-qa", "Move to QA", "The dashboard moves the finished task into the project's QA stage.")
            .detail("Runs after the agent signals completion. Resolution order: the `stage` set here, then config.doneStage, then known QA names (\"Quality Assurance\", \"QA\", \"Ready for QA\", \"Testing\", \"UAT\"…), then a fuzzy QA match. If nothing matches, the task is deliberately left alone rather than advanced — guessing could land it in \"Revision Required\"."),
        StepDef::new("finish", "Finish", "Signals completion so the dashboard can update Odoo.", finish)
            .detail("The agent runs the completion command; the dashboard then posts the summary and MR link, archives the transcript, and writes the standup line. This is the step a project most often customises — see `run` in a project pipeline.json."),
    ],
};

fn context(vars: &PromptVars) -> String {
    let prior = match vars.archive_path.as_deref() {
        Some(path) => format!(
            " This task already has prior work: before implementing, assess the existing GitLab branch, any open MR, and the current code, and do NOT redo completed work. A transcript of the earlier session (decisions, reasoning, files touched) is at {path} — read it if you need that context."
        ),
        None => String::new(),
    };
    format!("{}{prior}", vars.extra_context_sentence())
}

fn readiness_gate(vars: &PromptVars) -> String {
    format!(
        " STEP 0 — READINESS GATE (do this before anything else): run /task-readiness-gate for {url}. \
If the verdict is BLOCKED_NEEDS_INFO, do NOT modify any code: post the clarifying questions to the task, \
then run `{blocked}` to flag it on my dashboard, and STOP — do not run /odoo-review, /begin-odoo-task, or {bin} done. \
Only if the verdict is READY_FOR_REPRO, READY_BUT_NO_REPRO, or READY_NO_REPRO_NEEDED do you continue. \
For READY_FOR_REPRO, confirm/reproduce the problem before fixing; for READY_BUT_NO_REPRO, post your assumptions to the task and proceed carefully.",
        url = vars.url,
        blocked = blocked_command(vars.task_id, "q1 | q2 | q3"),
        bin = bin(),
    )
}

fn review(vars: &PromptVars) -> String {
    format!(
        " Once the gate passes: run /odoo-review for {} to explore the code and post an implementation plan,",
        vars.url
    )
}

fn implement(_vars: &PromptVars) -> String {
    " then run /begin-odoo-task to implement it.".to_string()
}

fn open_mr(vars: &PromptVars) -> String {
    format!(
        " You MUST complete begin-odoo-task all the way through its final step: commit, push the branch, AND open a merge request with glab (targeting {}). Do NOT stop after just pushing — pushing without an MR is incomplete. Verify the MR URL exists before finishing.",
        vars.branch_instruction
    )
}

fn escalate(_vars: &PromptVars) -> String {
    format!(
        " If you get blocked and need a decision or missing context, notify me by running: {} (then pause and wait).",
        notify_command("short ask", "details")
    )
}

fn summary(vars: &PromptVars) -> String {
    format!(
        " Once the merge request is open, write a short plain-English summary to {} using EXACTLY these three markdown sections — \"**Root cause:**\" (what was actually wrong, in plain language a non-engineer can follow), \"**Fix:**\" (what you changed and why), and \"**Before vs after:**\" (the previous behavior/output versus the new behavior/output). Keep it concise (a few sentences each; bullet points are fine).",
        vars.summary_file
    )
}

fn journal(_vars: &PromptVars) -> String {
    " If this task involved non-trivial investigation — debugging across multiple files or systems, a wrong first assumption, a correction, hidden business rules, or a reusable lesson likely to recur — also run /problem-reasoning-journal to capture how the solution was found (root cause, wrong turns, and the reusable rule); skip it for trivial or one-line changes.".to_string()
}

fn finish(vars: &PromptVars) -> String {
    format!(
        " Then mark it done by running: {}",
        done_command(vars.task_id, &vars.summary_file)
    )
}
