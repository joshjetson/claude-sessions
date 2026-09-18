//! Project overrides: what `<repo>/.claude-sessions/pipeline.json` can change,
//! and — the part that cost real data — what it must not silently lose.

use serde_json::json;

use crate::pipeline::{resolve_pipeline, PipelineSource, PromptVars, TASK_PIPELINE};

use super::golden::{GOLDEN_REVISION, GOLDEN_TASK};
use super::{vars, Repo};

fn prompt_for(repo: &Repo) -> String {
    super::normalise_bin(
        &resolve_pipeline("task", Some(repo.path()))
            .unwrap()
            .build_prompt(&vars()),
    )
}

#[test]
fn no_override_file_means_the_built_in_pipeline() {
    let repo = Repo::empty();
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert_eq!(resolved.source, PipelineSource::BuiltIn);
    assert_eq!(resolved.steps.len(), TASK_PIPELINE.steps.len());
    assert!(resolved.steps.iter().all(|step| !step.customised));
    assert_eq!(
        super::normalise_bin(&resolved.build_prompt(&vars())),
        GOLDEN_TASK.concat()
    );
}

#[test]
fn skip_drops_a_step_from_the_prompt_but_keeps_it_in_the_diagram() {
    let repo = Repo::with(json!({ "extends": "task", "steps": { "journal": { "skip": true } } }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert_eq!(
        resolved.steps.len(),
        TASK_PIPELINE.steps.len(),
        "skipped steps must still be drawn"
    );
    assert!(resolved.step("journal").unwrap().skipped);
    assert_eq!(resolved.active_steps().len(), TASK_PIPELINE.steps.len() - 1);
    assert!(!resolved
        .build_prompt(&vars())
        .contains("problem-reasoning-journal"));
}

#[test]
fn run_appends_a_project_command_to_a_step() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "finish": { "run": "./scripts/notes.sh {{taskId}}" } },
    }));
    let out = prompt_for(&repo);
    assert!(
        out.contains("`./scripts/notes.sh 5944`"),
        "the command was not interpolated into the prompt"
    );
    // The original instruction must survive — run adds, it does not replace.
    assert!(out.contains("claude-sessions done 5944"));
    assert!(out.contains("(from the repository root) and report its outcome."));
}

#[test]
fn prompt_replaces_a_step_fragment_outright() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "review": { "prompt": "Skip review; go straight to code." } },
    }));
    let out = prompt_for(&repo);
    assert!(out.contains(" Skip review; go straight to code."));
    // The gate still mentions /odoo-review by name ("do not run /odoo-review"),
    // so check the review step's own wording is gone rather than the string.
    assert!(
        !out.contains("to explore the code and post an implementation plan"),
        "the replaced fragment still ran"
    );
}

#[test]
fn append_adds_to_a_step_without_losing_it() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "open-mr": { "append": "Also add the QA label." } },
    }));
    let out = prompt_for(&repo);
    assert!(out.contains("Also add the QA label."));
    assert!(out.contains("pushing without an MR is incomplete"));
}

#[test]
fn append_keeps_the_completion_instruction_and_prompt_loses_it() {
    // This is the distinction that cost real data: two projects replaced the
    // finish step and silently lost completion, QA moves, standup lines and
    // archived transcripts.
    let appended = Repo::with(json!({
        "extends": "task",
        "steps": { "finish": { "append": "Merge to development when done." } },
    }));
    let replaced = Repo::with(json!({
        "extends": "task",
        "steps": { "finish": { "prompt": "Merge to development when done." } },
    }));

    let with_append = prompt_for(&appended);
    let with_prompt = prompt_for(&replaced);

    assert!(
        with_append.contains("mark it done by running"),
        "append dropped the completion signal"
    );
    assert!(
        with_append.contains("Merge to development when done"),
        "append lost the project instruction"
    );
    assert!(
        !with_prompt.contains("mark it done by running"),
        "prompt no longer replaces — the docs say it does"
    );
    assert!(with_prompt.contains("Merge to development when done"));
}

#[test]
fn a_new_step_lands_where_after_or_before_says() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": {
            "smoke": { "after": "open-mr", "title": "Smoke test", "prompt": "Run ./smoke.sh." },
            "lint": { "before": "implement", "title": "Lint", "prompt": "Run the linter." },
        },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    let ids: Vec<&str> = resolved.steps.iter().map(|step| step.id.as_str()).collect();
    let at = |needle: &str| ids.iter().position(|id| *id == needle).unwrap();
    assert_eq!(ids[at("open-mr") + 1], "smoke");
    assert_eq!(ids[at("implement") - 1], "lint");
}

#[test]
fn a_new_step_with_no_anchor_goes_last_after_finish() {
    // Documented explicitly because a project hit it: a "verify it works" step
    // landed after the deploy instruction.
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "extra": { "title": "Extra", "prompt": "Do the extra thing." } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert_eq!(resolved.steps.last().unwrap().id, "extra");
    assert_eq!(resolved.active_steps().last().unwrap().id, "extra");
}

#[test]
fn before_finish_puts_it_where_you_meant() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "smoke": { "before": "finish", "title": "Smoke", "prompt": "Run smoke." } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    let ids: Vec<&str> = resolved
        .active_steps()
        .iter()
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(ids[ids.len() - 1], "finish");
    assert_eq!(ids[ids.len() - 2], "smoke");
}

#[test]
fn added_and_customised_steps_are_flagged_for_the_viewers() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": {
            "finish": { "run": "./x.sh" },
            "smoke": { "after": "open-mr", "title": "Smoke" },
        },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert!(resolved.step("finish").unwrap().customised);
    assert!(!resolved.step("finish").unwrap().added);
    assert!(resolved.step("smoke").unwrap().added);
    assert_eq!(resolved.source, PipelineSource::Project);
}

#[test]
fn vars_supply_defaults_the_run_can_still_override() {
    let repo = Repo::with(json!({
        "extends": "task",
        "vars": { "targetBranch": "release" },
        "steps": { "finish": { "run": "ship {{targetBranch}}" } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();

    let not_supplied = PromptVars {
        target_branch: None,
        ..vars()
    };
    assert!(resolved
        .build_prompt(&not_supplied)
        .contains("ship release"));
    // An explicit run-time value wins over the project default.
    let supplied = PromptVars {
        target_branch: Some("hotfix".to_string()),
        ..vars()
    };
    assert!(resolved.build_prompt(&supplied).contains("ship hotfix"));
}

#[test]
fn an_override_for_another_pipeline_does_not_apply() {
    let repo = Repo::with(json!({
        "extends": "revision",
        "steps": { "journal": { "skip": true } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert_eq!(resolved.source, PipelineSource::BuiltIn);
    assert_eq!(
        super::normalise_bin(&resolved.build_prompt(&vars())),
        GOLDEN_TASK.concat()
    );

    // …and the pipeline it does name still gets it.
    let revision = resolve_pipeline("revision", Some(repo.path())).unwrap();
    assert!(revision.step("journal").unwrap().skipped);
}

#[test]
fn an_override_with_no_extends_applies_to_whatever_is_resolved() {
    let repo = Repo::with(json!({ "steps": { "journal": { "skip": true } } }));
    assert!(
        resolve_pipeline("task", Some(repo.path()))
            .unwrap()
            .step("journal")
            .unwrap()
            .skipped
    );
    assert!(
        resolve_pipeline("revision", Some(repo.path()))
            .unwrap()
            .step("journal")
            .unwrap()
            .skipped
    );
}

#[test]
fn a_patch_for_an_unknown_step_id_is_ignored_not_fatal() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "no-such-step": { "skip": true } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    // It becomes a new (empty) step rather than throwing, exactly as before.
    assert!(resolved.step("no-such-step").is_some());
    assert!(resolved
        .build_prompt(&vars())
        .starts_with("Work this Odoo task"));
}

#[test]
fn malformed_json_reports_the_problem_instead_of_crashing_the_dashboard() {
    let repo = Repo::with_text("{ this is not json");
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert!(
        resolved.override_error.is_some(),
        "the parse failure was swallowed"
    );
    assert_eq!(
        resolved.source,
        PipelineSource::BuiltIn,
        "a broken override must fall back to the default"
    );
    // And the prompt still builds.
    assert_eq!(
        super::normalise_bin(&resolved.build_prompt(&vars())),
        GOLDEN_TASK.concat()
    );
    assert_eq!(resolved.override_file.unwrap(), repo.pipeline_file());
}

#[test]
fn the_revision_pipeline_resolves_the_same_way() {
    let repo = Repo::empty();
    assert_eq!(
        super::normalise_bin(
            &resolve_pipeline("revision", Some(repo.path()))
                .unwrap()
                .build_prompt(&vars()),
        ),
        GOLDEN_REVISION.concat()
    );
}
