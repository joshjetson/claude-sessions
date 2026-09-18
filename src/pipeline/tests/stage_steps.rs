//! Stage-move steps, the `$`-prefixed documentation keys, and interpolation.
//!
//! Moving a task between stages is something the DASHBOARD does, not something
//! the agent is told to do. Modelling it as a step makes it visible in the flow
//! and overridable per project — and the dashboard reads these definitions, so
//! what the diagram shows is what actually happens.

use std::collections::BTreeMap;

use serde_json::json;

use crate::pipeline::{
    dashboard_step, interpolate, read_project_override, resolve_pipeline, PipelineSource,
};

use super::golden::GOLDEN_TASK;
use super::{vars, Repo};

#[test]
fn dashboard_step_reports_the_default_move_using_the_usual_resolution() {
    let step = dashboard_step("task", None, "move-qa");
    assert!(step.enabled);
    assert_eq!(step.stage, None);
}

#[test]
fn a_project_can_turn_a_move_off() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "move-in-progress": { "skip": true } },
    }));
    assert!(!dashboard_step("task", Some(repo.path()), "move-in-progress").enabled);
    // …without affecting the other move.
    assert!(dashboard_step("task", Some(repo.path()), "move-qa").enabled);
}

#[test]
fn a_project_can_send_finished_tasks_to_a_different_stage() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "move-qa": { "stage": "Testing" } },
    }));
    let step = dashboard_step("task", Some(repo.path()), "move-qa");
    assert!(step.enabled);
    assert_eq!(step.stage.as_deref(), Some("Testing"));
}

#[test]
fn skipping_a_move_still_draws_it_marked_skipped() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "move-qa": { "skip": true } },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    let step = resolved.step("move-qa").unwrap();
    assert!(step.skipped);
    assert!(step.customised);
}

#[test]
fn an_unreadable_override_falls_back_to_moving_not_to_doing_nothing() {
    // Failing closed here would silently strand every finished task in progress.
    let repo = Repo::with_text("{ not json");
    assert!(dashboard_step("task", Some(repo.path()), "move-qa").enabled);
}

#[test]
fn an_unknown_step_id_means_do_the_default_not_skip() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "move-qa": { "skip": true } },
    }));
    assert!(dashboard_step("task", Some(repo.path()), "no-such-step").enabled);
}

#[test]
fn a_bad_pipeline_id_degrades_to_the_default_rather_than_throwing() {
    assert!(dashboard_step("nope", None, "move-qa").enabled);
}

// --- $-prefixed keys are documentation, not overrides -----------------------

#[test]
fn only_the_step_you_really_edit_is_marked() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": {
            "review": { "$what": "note", "$edit": "note" },
            "move-qa": { "$what": "note", "stage": "Testing" },
        },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    let customised: Vec<&str> = resolved
        .steps
        .iter()
        .filter(|step| step.customised)
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(customised, vec!["move-qa"]);
    assert_eq!(resolved.source, PipelineSource::Project);
}

#[test]
fn dollar_keys_never_become_overrides() {
    let repo = Repo::with(json!({
        "extends": "task",
        "steps": { "move-qa": { "$stage": "Testing", "$skip": true } },
    }));
    let step = dashboard_step("task", Some(repo.path()), "move-qa");
    assert_eq!(step.stage, None, "$stage was treated as a real override");
    assert!(step.enabled, "$skip was treated as a real override");
}

#[test]
fn dollar_keys_in_vars_are_ignored_too() {
    let repo = Repo::with(json!({
        "extends": "task",
        "vars": { "$example": "targetBranch: \"main\"" },
    }));
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert!(resolved.vars.is_empty());
    assert_eq!(
        super::normalise_bin(&resolved.build_prompt(&vars())),
        GOLDEN_TASK.concat()
    );
}

#[test]
fn read_project_override_exposes_the_raw_vars() {
    let repo = Repo::with(json!({ "extends": "task", "vars": { "keep": "me" } }));
    let over = read_project_override(Some(repo.path())).unwrap();
    assert_eq!(over.vars.get("keep").unwrap(), "me");
    assert_eq!(over.extends.as_deref(), Some("task"));
    assert!(read_project_override(None).is_none());
    assert!(read_project_override(Some(Repo::empty().path())).is_none());
}

// --- interpolation ----------------------------------------------------------

#[test]
fn interpolate_fills_known_variables_and_leaves_unknown_ones_visible() {
    let empty = BTreeMap::new();
    assert_eq!(
        interpolate("task {{taskId}} on {{branch}}", &vars(), &empty),
        "task 5944 on {{branch}}"
    );
}

#[test]
fn interpolate_tolerates_whitespace_inside_the_braces() {
    let empty = BTreeMap::new();
    assert_eq!(interpolate("{{ taskId }}", &vars(), &empty), "5944");
}

#[test]
fn interpolate_reads_project_vars_when_the_run_supplied_nothing() {
    let mut project = BTreeMap::new();
    project.insert("region".to_string(), "eu".to_string());
    assert_eq!(
        interpolate("deploy to {{region}}", &vars(), &project),
        "deploy to eu"
    );
}

#[test]
fn interpolate_leaves_an_unterminated_placeholder_alone() {
    let empty = BTreeMap::new();
    assert_eq!(interpolate("run {{taskId", &vars(), &empty), "run {{taskId");
}
