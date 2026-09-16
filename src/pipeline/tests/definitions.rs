//! What every pipeline definition must be true of, whatever else changes.

use std::collections::HashSet;

use crate::pipeline::{
    pipeline_skills, resolve_pipeline, StepKind, BUILT_IN, REVISION_PIPELINE, TASK_PIPELINE,
};

use super::{golden::conflict_vars, vars};

#[test]
fn every_step_has_the_fields_both_viewers_render() {
    for pipeline in BUILT_IN {
        for step in pipeline.steps {
            assert!(!step.id.is_empty(), "{}: step missing id", pipeline.id);
            assert!(
                !step.title.is_empty(),
                "{}/{}: missing title",
                pipeline.id,
                step.id
            );
            assert!(
                !step.what.is_empty(),
                "{}/{}: missing what",
                pipeline.id,
                step.id
            );
        }
    }
}

#[test]
fn step_ids_are_unique_within_a_pipeline() {
    for pipeline in BUILT_IN {
        let mut seen = HashSet::new();
        for step in pipeline.steps {
            assert!(
                seen.insert(step.id),
                "{} has duplicate step id {}",
                pipeline.id,
                step.id
            );
        }
    }
}

#[test]
fn pipeline_ids_are_unique() {
    let mut seen = HashSet::new();
    for pipeline in BUILT_IN {
        assert!(
            seen.insert(pipeline.id),
            "duplicate pipeline {}",
            pipeline.id
        );
    }
}

#[test]
fn every_declared_skill_is_actually_named_in_its_prompt_fragment() {
    // A step advertising /odoo-review in the diagram while telling the agent
    // something else would be exactly the drift this design exists to prevent.
    let vars = conflict_vars(true);
    for pipeline in BUILT_IN {
        for step in pipeline.steps {
            let Some(skill) = step.skill else { continue };
            let fragment = (step.fragment)(&vars);
            assert!(
                fragment.contains(&format!("/{skill}")),
                "{}/{} claims skill {skill} but its prompt never invokes it",
                pipeline.id,
                step.id
            );
        }
    }
}

#[test]
fn the_declared_skills_are_the_ones_the_pipelines_invoke() {
    // Derived from the definitions rather than hand-listed — the Node list had
    // already drifted from what the pipelines actually ran.
    assert_eq!(
        pipeline_skills(),
        vec![
            "begin-odoo-task",
            "handle-revision",
            "odoo-review",
            "pre-optics",
            "problem-reasoning-journal",
            "qa",
            "resolve-merge-conflict",
            "task-readiness-gate",
        ]
    );
}

#[test]
fn the_task_pipeline_gates_before_it_touches_code() {
    let ids: Vec<&str> = TASK_PIPELINE.steps.iter().map(|step| step.id).collect();
    assert!(
        ids.iter().position(|id| *id == "readiness-gate")
            < ids.iter().position(|id| *id == "implement")
    );
    assert!(
        TASK_PIPELINE
            .steps
            .iter()
            .find(|step| step.id == "readiness-gate")
            .unwrap()
            .gate
    );
}

#[test]
fn both_pipelines_end_by_signalling_completion() {
    for pipeline in [&TASK_PIPELINE, &REVISION_PIPELINE] {
        assert_eq!(
            pipeline.steps.last().unwrap().id,
            "finish",
            "{} does not end on finish",
            pipeline.id
        );
    }
}

#[test]
fn both_pipelines_move_to_a_working_stage_and_then_to_qa() {
    for id in ["task", "revision"] {
        let resolved = resolve_pipeline(id, None).unwrap();
        let ids: Vec<&str> = resolved.steps.iter().map(|step| step.id.as_str()).collect();
        let at = |needle: &str| ids.iter().position(|step| *step == needle);
        assert!(
            at("move-in-progress").is_some(),
            "{id} has no move-in-progress"
        );
        assert!(at("move-qa").is_some(), "{id} has no move-qa");
        assert!(at("move-in-progress") < at("move-qa"));
        // QA comes before the agent signals done, so the board is correct by then.
        assert!(at("move-qa") < at("finish"));
    }
}

#[test]
fn stage_moves_are_dashboard_steps_that_contribute_nothing_to_the_prompt() {
    // The agent must never be instructed to move the task itself.
    let resolved = resolve_pipeline("task", None).unwrap();
    for id in ["move-in-progress", "move-qa"] {
        let step = resolved.step(id).unwrap();
        assert_eq!(step.kind, StepKind::Dashboard);
        assert_eq!(resolved.prompt_for(step, &vars()), "");
    }
}

#[test]
fn qa_never_moves_a_stage_at_all() {
    // The QA stage IS the working stage; a move would encode the same fact
    // twice and go stale if the session died.
    for id in ["qa", "qa-dry", "pre-optics"] {
        let resolved = resolve_pipeline(id, None).unwrap();
        assert!(
            resolved
                .steps
                .iter()
                .all(|step| step.kind == StepKind::Agent),
            "{id} has a dashboard step"
        );
    }
}

#[test]
fn an_unknown_pipeline_id_is_an_error_rather_than_an_empty_prompt() {
    let err = resolve_pipeline("nope", None).unwrap_err();
    assert_eq!(err.to_string(), "Unknown pipeline \"nope\"");
}
