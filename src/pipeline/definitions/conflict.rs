//! Resolve the merge conflicts blocking a task's MR.
//!
//! In the Node app this was a twelve-line template literal hardcoded in
//! `tui/actions.js`, which is exactly the thing the pipeline module exists to
//! replace: it could not be seen in the viewer, documented, or overridden by a
//! project. It is a pipeline here.
//!
//! `resumed` means the session that wrote this branch is being re-entered, so
//! it already knows why every hunk looks the way it does and the prompt leans on
//! that instead of re-deriving intent from the diff. Without it the agent
//! reconstructs intent from the task and the conflicting commits, which is what
//! /resolve-merge-conflict is for.

use super::{PipelineDef, StepDef};
use crate::pipeline::vars::{notify_command, PromptVars};

pub static CONFLICT_PIPELINE: PipelineDef = PipelineDef {
    id: "conflict",
    name: "Resolve merge conflicts",
    trigger: "Enter on a deploy task whose MR conflicts",
    summary: "Reconciles a conflicted merge request against what has landed since, keeping both sides' intent, and hands the MR back green without merging it.",
    preamble: "The merge request for this task has merge conflicts and cannot be merged.",
    steps: &[
        StepDef::new("context", "Prior context", "Says how much the agent already knows about this branch.", context)
            .detail("Three cases: a resumed session wrote the branch itself; an archived transcript of the session that did exists; or neither, in which case intent has to be established from the task and the commit history before anything is reconciled."),
        StepDef::new("mr", "Name the merge request", "States which MR conflicts and which branches it joins.", merge_request)
            .detail("The agent needs the source and target branches by name — the whole reconciliation is described in terms of them."),
        StepDef::new("resolve", "Reconcile every hunk", "Merges the target branch in and resolves each conflicting hunk so both intents survive.", resolve)
            .skill("resolve-merge-conflict")
            .detail("Never resolve by taking one side wholesale: a hunk conflicts precisely because another task changed the same code, so the result has to satisfy both. This is the instruction the step exists for."),
        StepDef::new("escalate", "Ask rather than guess", "Stops and notifies when a hunk has no determinable reconciliation.", escalate)
            .gate()
            .detail("A wrong guess here silently reverts somebody else's task. The notification reaches the dashboard feed and the session waits."),
        StepDef::new("verify", "Build, test, push", "Confirms the project still builds and the MR reports mergeable again.", verify)
            .detail("Pushing a merge that does not build turns one blocked MR into two."),
        StepDef::new("hand-back", "Hand it back", "Reports what was reconciled and leaves the merge to a human.", hand_back)
            .detail("The dashboard merges from the deploy tab once the MR is green; an agent merging on its own removes the review the tab exists to provide."),
    ],
};

fn context(vars: &PromptVars) -> String {
    let context = if vars.resumed {
        " You have full prior context from this resumed session — the changes on this branch are yours, along with the reasoning behind them.".to_string()
    } else {
        match vars.archive_path.as_deref() {
            Some(path) => format!(
                " A transcript of the session that produced this branch is at {path} — read it first to recover the intent behind these changes."
            ),
            None => " You did not write this branch, so establish intent from the Odoo task and the commit history before resolving anything.".to_string(),
        }
    };
    format!("{}{context}", vars.extra_context_sentence())
}

fn merge_request(vars: &PromptVars) -> String {
    let mr = vars.merge_request();
    format!(
        " MR !{iid} ({url}) merges `{source}` into `{target}`.",
        iid = mr.iid,
        url = mr.url,
        source = mr.source_branch,
        target = mr.target_branch,
    )
}

fn resolve(vars: &PromptVars) -> String {
    let mr = vars.merge_request();
    format!(
        " Resolve the conflicts by running /resolve-merge-conflict for {url}. \
Concretely: fetch the latest `{target}`, merge it into `{source}`, and reconcile every conflicting hunk — \
keeping BOTH this task's intent and whatever landed on `{target}` in the meantime. \
Never resolve a conflict by blindly taking one side: if a hunk conflicts because another task changed the same code, the result must satisfy both.",
        url = vars.url,
        target = mr.target_branch,
        source = mr.source_branch,
    )
}

fn escalate(_vars: &PromptVars) -> String {
    format!(
        " If you cannot determine the correct reconciliation for a hunk, stop and ask me rather than guessing — run: {} (then pause and wait).",
        notify_command(
            "merge conflict needs a decision",
            "which behavior should win, and why"
        )
    )
}

fn verify(vars: &PromptVars) -> String {
    let mr = vars.merge_request();
    format!(
        " After resolving, make sure the project still builds and its tests pass, then commit the merge and push to `{source}`. \
Finally, confirm with glab that MR !{iid} no longer reports conflicts and reports as mergeable.",
        source = mr.source_branch,
        iid = mr.iid,
    )
}

fn hand_back(_vars: &PromptVars) -> String {
    " Do NOT merge the MR yourself — I'll do that from my dashboard once it's green. When the branch is pushed and the MR is clean, tell me in one line what you reconciled and why.".to_string()
}
