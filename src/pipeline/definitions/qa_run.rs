//! Coordinating a QA run: watching several passes at once.
//!
//! It is not a QA pass and must never become one. Four rules shape the prompt,
//! and each exists because the obvious alternative is worse:
//!
//! 1. It never runs `/qa` itself. A session that both reviews and coordinates
//!    fills its context with one task's detail and stops being able to see the
//!    other six.
//! 2. It never spawns. Admission — how many sessions may run at once — is
//!    decided outside any model, because a backstop a model can raise is not a
//!    backstop.
//! 3. It never answers a verdict checkpoint, in either mode. QAden stops `/qa`
//!    there by design; a coordinator answering it would remove that checkpoint
//!    more quietly than deleting it would. The answer policy enforces this
//!    independently of the prompt.
//! 4. It never writes to Odoo. Same rule as the QA pipeline, for the same
//!    reason: a wrong note goes out under the reviewer's name.
//!
//! Shadow and triage differ in one step. Both record what the coordinator would
//! have answered BEFORE acting on it, so the agreement rate accumulates either
//! way; shadow then stops, and triage also delivers answers of fact.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::{
    notify_kind_command, qa_answer_command, qa_shadow_command, PromptVars,
};

/// The key a caller sets to turn triage on. Absent means shadow, which is the
/// mode that cannot be wrong in a way a QA pass would not notice.
pub const TRIAGE_VAR: &str = "triage";
/// The run this coordinator is watching.
pub const RUN_ID_VAR: &str = "runId";
/// Comma-separated task ids, fixed at launch.
pub const TASK_IDS_VAR: &str = "taskIds";
/// Where QAden records each task's state.
pub const QA_ROOT_VAR: &str = "qaRoot";

pub static QA_RUN_PIPELINE: PipelineDef = PipelineDef {
    id: "qa-run",
    name: "Coordinate a QA run",
    trigger: "Enter on a board QA run → Start coordinator",
    summary: "Watches several QA passes at once. In shadow mode it records what it would have answered and escalates everything. In triage mode it also answers questions of fact, and still escalates every judgment call and every verdict. Posts nothing to Odoo, spawns nothing.",
    preamble: "You are coordinating a QA run: several independent QA sessions working different tasks at the same time. You are not reviewing any of them. Your value is that you can see all of them at once and a person cannot watch seven terminals — so be accurate about what is happening, and never smooth over a session you do not understand.",
    steps: &[
        StepDef::new(
            "context",
            "Run context",
            "Hands over the run, its tasks, and where their state lives.",
            context,
        )
        .detail("The task list is fixed at launch. A task that fails QA leaves the stage and stays in the run, so the coordinator is told its own list rather than re-reading the stage."),

        StepDef::new(
            "no-qa",
            "Do not run QA yourself",
            "Keeps the coordinator a coordinator.",
            no_qa,
        )
        .detail("A session that reviews one task in depth loses the ability to watch the other six. If a pass needs doing, it belongs in its own session."),

        StepDef::new(
            "triage",
            "Answer, or escalate",
            "Answers what it can from project context and escalates the rest.",
            triage,
        )
        .gate()
        .detail("Both modes record what the coordinator would have said BEFORE acting on it, so the agreement rate accumulates either way. In shadow mode it stops there. In triage mode it also delivers the answer, and the daemon decides whether that answer may be delivered at all."),

        StepDef::new(
            "verdict-never",
            "Never touch a verdict",
            "The one thing that stays human in every mode.",
            verdict_never,
        )
        .detail("Not a shadow-mode restriction. QAden stops /qa at a human checkpoint by design, and a coordinator answering it would remove that checkpoint without anyone deciding to. Enforced by the answer policy as well as stated here."),

        StepDef::new(
            "no-odoo",
            "Post nothing",
            "Odoo is untouched for the whole run.",
            no_odoo,
        )
        .detail("Same rule as the QA pipeline. Several verdicts under the reviewer's name is several times the reason to keep it."),

        StepDef::new(
            "report",
            "Report the run",
            "One status line per task, on request.",
            report,
        )
        .detail("The board already renders the run block. This is for the conversation pane, where the reasoning behind a status belongs."),
    ],
};

fn run_id(vars: &PromptVars) -> &str {
    vars.extras
        .get(RUN_ID_VAR)
        .map_or("this run", String::as_str)
}

fn context(vars: &PromptVars) -> String {
    let ids = vars
        .extras
        .get(TASK_IDS_VAR)
        .map_or("(none listed)", String::as_str);
    let qa_root = vars
        .extras
        .get(QA_ROOT_VAR)
        .map_or("the QA directory", String::as_str);

    let extra = if vars.extra_context.trim().is_empty() {
        String::new()
    } else {
        format!(
            " IMPORTANT extra context from the user — honor this above generic assumptions: {}.",
            vars.extra_context.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    };

    format!(
        " You are coordinating QA run \"{}\", covering tasks: {ids}. \
         Each task's QA state is recorded at {qa_root}/task-<id>-qa/run.json — read those files to \
         see progress. The `verdict` field is the authority on whether a pass finished: \"pass\", \
         \"revisions\", or absent while it is still open. Never infer a verdict from an agent's \
         prose.{extra}",
        run_id(vars)
    )
}

fn no_qa(_vars: &PromptVars) -> String {
    " Do NOT run /qa, /review-task or any QA skill yourself, and do not open the application. \
     You are watching passes, not performing one. If you find yourself reading a diff in detail, \
     stop — that is a signal the work belongs in its own session."
        .to_string()
}

fn triage(vars: &PromptVars) -> String {
    let id = run_id(vars);
    let record = format!(
        " For every question, FIRST record what you would answer: {}. \
         If you genuinely cannot answer, record `--would-answer \"I do not know: <why>\"` — that is \
         a real data point where an empty answer is not. Record BEFORE you act, never after, and \
         never try to edit a recorded answer: the command will refuse, and that refusal is what \
         makes the record worth keeping.",
        qa_shadow_command(id, vars.task_id)
    );

    let escalate = notify_kind_command(
        "QA run: #<id> needs you",
        "<the question, then what you would have said>",
        "warn",
        "question",
    );

    if vars.extras.get(TRIAGE_VAR).map(String::as_str) != Some("true") {
        return format!(
            " When a task's QA session asks a question, you do NOT answer it.{record} \
             Then escalate it to me: {escalate}."
        );
    }

    format!(
        " When a task's QA session asks a question, you triage it.{record} \
         Then decide, and be honest with yourself about which case you are in: \
         (a) it is a question of FACT you can settle from the task, the code, the merge request, \
         the project's config or its docs — which environment, which login, where a feature lives, \
         what a field means. Answer it: {}. \
         (b) it needs a JUDGMENT CALL, changes the scope of what is being tested, asks whether \
         something is acceptable, or you are not certain. Do NOT answer. Escalate it: {escalate}. \
         When you are between the two, escalate. A wrong answer sends a QA pass down a false trail \
         and the pass will not know to doubt you. If the answer command refuses, do not rephrase it \
         to get past the refusal — escalate instead, and say the refusal happened.",
        qa_answer_command(vars.task_id)
    )
}

fn verdict_never(_vars: &PromptVars) -> String {
    let escalate = notify_kind_command(
        "QA run: #<id> verdict needs you",
        "<what it is asking>",
        "warn",
        "verdict",
    );
    format!(
        " If a session reaches its verdict checkpoint, or asks you to confirm a PASS or a REVISION \
         REQUIRED, you must NOT answer it — not even to agree, not even when the answer looks \
         obvious, and not in triage mode. Escalate it unchanged: {escalate}. This rule does not \
         relax as you gain confidence. It is enforced outside this prompt as well — an answer sent \
         against a verdict is refused — so attempting it wastes a turn and tells you nothing you \
         did not already know."
    )
}

fn no_odoo(_vars: &PromptVars) -> String {
    " Do NOT post anything to Odoo, do NOT move any task to another stage, and do NOT tag anyone, \
     for any task in this run."
        .to_string()
}

fn report(vars: &PromptVars) -> String {
    format!(
        " When I ask for the state of the run, report one line per task: the id, what it is doing, \
         and anything outstanding. Say plainly which tasks you are unsure about. When every task in \
         run \"{}\" has a recorded verdict, tell me and stop — do not end the session.",
        run_id(vars)
    )
}
