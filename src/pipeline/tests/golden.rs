//! GOLDEN MASTERS — the assembled prompts must not drift.
//!
//! Every string here is the Node app's pinned prompt with exactly two kinds of
//! substitution applied, both forced by the single-binary decision:
//!
//! * `node /abs/path/bin/<done|blocked|notify>.js` becomes
//!   `claude-sessions <done|blocked|notify>`;
//! * the summary file moved from `/tmp/claude-sessions-task-<id>-summary.md` to
//!   the runtime directory (`Paths::task_summary_file`).
//!
//! Nothing else changed. Each golden is written as literal fragments joined with
//! nothing between them, so the leading space every fragment carries is visible
//! in the source — that space is part of the contract.

use crate::pipeline::{resolve_pipeline, MergeRequestVars, PromptVars};

use super::vars;

fn prompt(pipeline_id: &str, vars: &PromptVars) -> String {
    resolve_pipeline(pipeline_id, None)
        .expect("built-in pipeline")
        .build_prompt(vars)
}

pub(crate) const GOLDEN_TASK: &[&str] = &[
    "Work this Odoo task end-to-end.",
    " STEP 0 — READINESS GATE (do this before anything else): run /task-readiness-gate for https://odoo/web#id=5944&model=project.task&view_type=form. If the verdict is BLOCKED_NEEDS_INFO, do NOT modify any code: post the clarifying questions to the task, then run `claude-sessions blocked 5944 --questions \"q1 | q2 | q3\"` to flag it on my dashboard, and STOP — do not run /odoo-review, /begin-odoo-task, or claude-sessions done. Only if the verdict is READY_FOR_REPRO, READY_BUT_NO_REPRO, or READY_NO_REPRO_NEEDED do you continue. For READY_FOR_REPRO, confirm/reproduce the problem before fixing; for READY_BUT_NO_REPRO, post your assumptions to the task and proceed carefully.",
    " Once the gate passes: run /odoo-review for https://odoo/web#id=5944&model=project.task&view_type=form to explore the code and post an implementation plan,",
    " then run /begin-odoo-task to implement it.",
    " You MUST complete begin-odoo-task all the way through its final step: commit, push the branch, AND open a merge request with glab (targeting the `main` branch (this project targets `main`, NOT development)). Do NOT stop after just pushing — pushing without an MR is incomplete. Verify the MR URL exists before finishing.",
    " If you get blocked and need a decision or missing context, notify me by running: claude-sessions notify --title \"short ask\" --message \"details\" (then pause and wait).",
    " Once the merge request is open, write a short plain-English summary to /runtime/summaries/task-5944-summary.md using EXACTLY these three markdown sections — \"**Root cause:**\" (what was actually wrong, in plain language a non-engineer can follow), \"**Fix:**\" (what you changed and why), and \"**Before vs after:**\" (the previous behavior/output versus the new behavior/output). Keep it concise (a few sentences each; bullet points are fine).",
    " If this task involved non-trivial investigation — debugging across multiple files or systems, a wrong first assumption, a correction, hidden business rules, or a reusable lesson likely to recur — also run /problem-reasoning-journal to capture how the solution was found (root cause, wrong turns, and the reusable rule); skip it for trivial or one-line changes.",
    " Then mark it done by running: claude-sessions done 5944 --summary-file /runtime/summaries/task-5944-summary.md",
];

pub(crate) const GOLDEN_REVISION: &[&str] = &[
    "A revision was requested on this task. You have full prior context from this resumed session — your earlier decisions, reasoning, files investigated, and changes made.",
    " Run /handle-revision for https://odoo/web#id=5944&model=project.task&view_type=form to address the QA feedback.",
    " Commit and push your fixes, and make sure the existing merge request reflects them (push to the same branch; if no MR exists yet, open one with glab targeting the `main` branch (this project targets `main`, NOT development)).",
    " Once the revision is pushed and the MR is up to date, write a short plain-English summary to /runtime/summaries/task-5944-summary.md using EXACTLY these three markdown sections — \"**Root cause:**\" (what the QA issue actually was, in plain language), \"**Fix:**\" (what you changed and why), and \"**Before vs after:**\" (the previous behavior/output versus the new behavior/output). Keep it concise.",
    " A revision means the first attempt missed something — if there's a reusable lesson (a wrong assumption, a hidden business rule, or why QA caught this), run /problem-reasoning-journal to record it so it isn't repeated; skip it only for trivial fixes.",
    " Then run: claude-sessions done 5944 --summary-file /runtime/summaries/task-5944-summary.md",
];

const GOLDEN_QA: &[&str] = &[
    "You are the QA reviewer for this task, and you are starting cold on purpose. Do not assume the implementation is correct and do not reuse any reasoning from whoever built it — independence is the whole value of this pass.",
    " The task is https://odoo/web#id=5944&model=project.task&view_type=form.",
    " Run /qa 5944 and follow that skill exactly, including its completeness challenge. Do not skip the challenge and do not shorten the gap list after forming a verdict.",
    " When /qa has produced its note, do NOT post anything to Odoo, do NOT move the task to another stage, and do NOT tag anyone — leave the @PM placeholder exactly as written. Leave the note where write_note.py saved it in the task's QA directory. Then flag it for review by running: claude-sessions notify --title \"QA #5944: PASS\" (or \"QA #5944: REVISION REQUIRED\") --message \"<one line on the outcome, then the absolute path to the saved note>\" --level success (use --level warn when revisions are required). Finally, print the note in the terminal exactly as write_note.py emitted it, unfenced, then stop and wait for the user. Do not end the session. A human decides whether it is posted.",
];

/// The sixth pipeline: the prompt that was hardcoded in `tui/actions.js`.
const GOLDEN_CONFLICT: &[&str] = &[
    "The merge request for this task has merge conflicts and cannot be merged.",
    " You have full prior context from this resumed session — the changes on this branch are yours, along with the reasoning behind them.",
    " MR !403 (https://git.example/group/repo/-/merge_requests/403) merges `task-5944-fix` into `main`.",
    " Resolve the conflicts by running /resolve-merge-conflict for https://odoo/web#id=5944&model=project.task&view_type=form. Concretely: fetch the latest `main`, merge it into `task-5944-fix`, and reconcile every conflicting hunk — keeping BOTH this task's intent and whatever landed on `main` in the meantime. Never resolve a conflict by blindly taking one side: if a hunk conflicts because another task changed the same code, the result must satisfy both.",
    " If you cannot determine the correct reconciliation for a hunk, stop and ask me rather than guessing — run: claude-sessions notify --title \"merge conflict needs a decision\" --message \"which behavior should win, and why\" (then pause and wait).",
    " After resolving, make sure the project still builds and its tests pass, then commit the merge and push to `task-5944-fix`. Finally, confirm with glab that MR !403 no longer reports conflicts and reports as mergeable.",
    " Do NOT merge the MR yourself — I'll do that from my dashboard once it's green. When the branch is pushed and the MR is clean, tell me in one line what you reconciled and why.",
];

const GOLDEN_QA_DRY: &[&str] = &[
    "You are the QA reviewer for this task, and you are starting cold on purpose. Do not assume the implementation is correct and do not reuse any reasoning from whoever built it — independence is the whole value of this pass.",
    " The task is https://odoo/web#id=5944&model=project.task&view_type=form.",
    " Run /qa 5944 and follow that skill exactly, including its completeness challenge. Do not skip the challenge and do not shorten the gap list after forming a verdict.",
    " This is a dry run by the developer, not a hand-back. Do NOT write a pass note or a revision note, do NOT run write_note.py, do NOT notify the dashboard, and do NOT touch Odoo in any way. Report the per-criterion results and the challenge findings in the terminal, then stop and wait for the user. Do not end the session.",
];

const GOLDEN_PRE_OPTICS: &[&str] = &[
    "You are writing a pre-work brief for a task nobody has started yet. Your job is to work out what is actually being asked and what the task does not say — not to design the fix. A confident brief that is wrong costs a developer a day, so say plainly where you are uncertain rather than smoothing it over.",
    " The task is https://odoo/web#id=5944&model=project.task&view_type=form.",
    " Run /pre-optics 5944 and follow that skill exactly, including its clarifying-question round.",
    " Do NOT post anything to Odoo, do NOT move the task to another stage, and do NOT tag anyone. Then flag the brief for review by running: claude-sessions notify --title \"Pre-work brief #5944 ready\" --message \"<one line on what the task actually asks, then the Optics process name and the path to the brief>\" --level info. Finally print the brief in the terminal, unfenced, then stop and wait for the user. Do not end the session.",
];

pub(crate) fn conflict_vars(resumed: bool) -> PromptVars {
    PromptVars {
        resumed,
        mr: Some(MergeRequestVars {
            iid: 403,
            url: "https://git.example/group/repo/-/merge_requests/403".to_string(),
            source_branch: "task-5944-fix".to_string(),
            target_branch: "main".to_string(),
        }),
        ..vars()
    }
}

#[test]
fn task_pipeline_matches_the_prompt_the_dashboard_has_always_sent() {
    assert_eq!(prompt("task", &vars()), GOLDEN_TASK.concat());
}

#[test]
fn revision_pipeline_matches_the_prompt_the_dashboard_has_always_sent() {
    assert_eq!(prompt("revision", &vars()), GOLDEN_REVISION.concat());
}

#[test]
fn qa_pipeline_posts_nothing_and_parks_its_verdict() {
    assert_eq!(prompt("qa", &vars()), GOLDEN_QA.concat());
}

#[test]
fn the_conflict_prompt_lifted_out_of_actions_js_is_unchanged() {
    assert_eq!(
        prompt("conflict", &conflict_vars(true)),
        GOLDEN_CONFLICT.concat()
    );
}

#[test]
fn a_dry_run_reports_instead_of_parking_a_verdict() {
    let out = prompt("qa-dry", &vars());
    assert_eq!(out, GOLDEN_QA_DRY.concat());
    // The same cold start and the same pass as `qa`…
    assert!(out.contains(" Run /qa 5944 and follow that skill exactly"));
    // …but nothing is written for hand-back and nothing is flagged.
    assert!(!out.contains("claude-sessions notify"));
    assert!(!out.contains("@PM placeholder"));
}

#[test]
fn the_pre_work_brief_prints_and_notifies_but_never_writes_to_odoo() {
    let out = prompt("pre-optics", &vars());
    assert_eq!(out, GOLDEN_PRE_OPTICS.concat());
    assert!(out.contains("Do NOT post anything to Odoo"));
}

#[test]
fn extra_context_is_injected_where_it_always_was() {
    let out = prompt(
        "task",
        &PromptVars {
            extra_context: "only  the modal".to_string(),
            ..vars()
        },
    );
    assert!(
        out.contains("IMPORTANT extra context from the user — honor this above generic assumptions: only the modal."),
        "extra context wording, whitespace collapsing or position changed"
    );
    assert!(
        out.find("extra context") < out.find("STEP 0"),
        "context must come before the gate"
    );
}

#[test]
fn a_prior_archive_adds_the_do_not_redo_instruction() {
    let out = prompt(
        "task",
        &PromptVars {
            archive_path: Some("/a/b.jsonl".to_string()),
            ..vars()
        },
    );
    assert!(out.contains("/a/b.jsonl"));
    assert!(out.contains("do NOT redo completed work"));
}

#[test]
fn a_conflict_run_that_is_not_resumed_says_where_the_intent_lives() {
    let with_archive = prompt(
        "conflict",
        &PromptVars {
            archive_path: Some("/a/b.jsonl".to_string()),
            ..conflict_vars(false)
        },
    );
    assert!(with_archive.contains(
        " A transcript of the session that produced this branch is at /a/b.jsonl — read it first to recover the intent behind these changes."
    ));

    let cold = prompt("conflict", &conflict_vars(false));
    assert!(cold.contains(
        " You did not write this branch, so establish intent from the Odoo task and the commit history before resolving anything."
    ));
}

#[test]
fn the_prompts_name_the_binary_not_a_script_path() {
    // The single-binary decision: prompts bake `claude-sessions <subcommand>`,
    // never an absolute path to a helper script that exists only on the machine
    // that wrote the prompt.
    for pipeline_id in ["task", "revision", "qa", "qa-dry", "pre-optics", "conflict"] {
        let out = prompt(pipeline_id, &conflict_vars(true));
        assert!(
            !out.contains("node /"),
            "{pipeline_id} still shells out to node"
        );
        assert!(
            !out.contains("done.js") && !out.contains("blocked.js") && !out.contains("notify.js"),
            "{pipeline_id} still names a helper script"
        );
    }
}

#[test]
fn the_summary_path_comes_from_the_runtime_directory_not_tmp() {
    // Node hardcoded /tmp/claude-sessions-task-<id>-summary.md, which an
    // isolated run shared with the real dashboard.
    let paths = crate::paths::Paths::resolve(
        std::path::Path::new("/home/dev"),
        &crate::paths::PathEnv::default(),
    );
    let summary = paths.task_summary_file(5944);
    assert!(summary.starts_with(&paths.runtime_dir));
    assert_eq!(summary.file_name().unwrap(), "task-5944-summary.md");
}
