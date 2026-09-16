//! The starter override: written once, never clobbered, and inert until edited.

use std::fs;

use serde_json::{json, Value};

use crate::pipeline::{
    init_project_pipeline, resolve_pipeline, starter_template, step_line_number, PipelineSource,
    TASK_PIPELINE,
};

use super::golden::GOLDEN_TASK;
use super::{vars, Repo};

fn template() -> Value {
    serde_json::from_str(&starter_template(&TASK_PIPELINE))
        .expect("the documentation broke the JSON")
}

#[test]
fn init_writes_a_starter_that_lists_every_step() {
    let repo = Repo::empty();
    let file = init_project_pipeline(repo.path(), "task").expect("init");
    assert!(file.exists());

    let text = fs::read_to_string(&file).unwrap();
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written["extends"], json!("task"));

    // Every step, in pipeline order. Checked against the text rather than the
    // parsed object because a JSON map loses its order once parsed — and the
    // order is the point: the file should read like the flow it describes.
    let mut offset = 0;
    for step in TASK_PIPELINE.steps {
        let key = format!("\n    \"{}\": {{", step.id);
        let at = text[offset..]
            .find(&key)
            .unwrap_or_else(|| panic!("{} is missing or out of order in the template", step.id));
        offset += at + key.len();
    }
    assert_eq!(
        written["steps"].as_object().unwrap().len(),
        TASK_PIPELINE.steps.len()
    );
}

#[test]
fn the_starter_is_inert_it_changes_nothing_until_edited() {
    let repo = Repo::empty();
    init_project_pipeline(repo.path(), "task").unwrap();
    let resolved = resolve_pipeline("task", Some(repo.path())).unwrap();
    assert_eq!(
        resolved.build_prompt(&vars()),
        GOLDEN_TASK.concat(),
        "a freshly written template altered the prompt"
    );
    // The template lists every step with its $what/$edit notes. Counting those
    // as changes lit up the entire flow as "custom" before you edited anything,
    // which made the markers useless for spotting what you had actually changed.
    assert!(resolved.steps.iter().all(|step| !step.customised));
    assert_eq!(
        resolved.source,
        PipelineSource::BuiltIn,
        "an untouched template must read as the default"
    );
}

#[test]
fn init_never_clobbers_an_existing_override() {
    let repo = Repo::with(json!({ "extends": "task", "vars": { "keep": "me" } }));
    let err = init_project_pipeline(repo.path(), "task").unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");

    let after: Value =
        serde_json::from_str(&fs::read_to_string(repo.pipeline_file()).unwrap()).unwrap();
    assert_eq!(after["vars"]["keep"], json!("me"));
}

#[test]
fn init_of_an_unknown_pipeline_writes_nothing() {
    let repo = Repo::empty();
    let err = init_project_pipeline(repo.path(), "nope").unwrap_err();
    assert!(err.to_string().contains("Unknown pipeline"), "{err}");
    assert!(!repo.pipeline_file().exists());
}

#[test]
fn the_starter_template_explains_the_distinction_that_matters() {
    // The file you land in when you press `e`. If someone rewrites it, this
    // keeps the warning that cost an epic's worth of archived context.
    let doc = template()["$doc"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        doc.contains("\"append\" keeps"),
        "the template no longer explains append"
    );
    assert!(
        doc.contains("\"prompt\" REPLACES"),
        "the template no longer warns that prompt replaces"
    );
    // Node said `done.js` here; the single binary renamed the command, not the
    // warning.
    assert!(
        doc.contains("claude-sessions done"),
        "the template no longer says what finish does"
    );
    assert!(
        doc.contains("ARCHIVE"),
        "the template no longer says archiving is at stake"
    );
    assert!(
        doc.contains("goes LAST"),
        "the template no longer warns about step ordering"
    );
}

#[test]
fn the_starter_template_is_still_valid_inert_json() {
    // Deliberately parsed rather than eyeballed: the help text is full of
    // quotes, and a broken file would take the whole pipeline down.
    let parsed = template();
    assert!(parsed["$doc"].is_array());
    assert!(parsed["vars"]["$example"].is_string());
}

#[test]
fn every_step_carries_its_own_hints_and_stage_steps_get_stage_hints() {
    let parsed = template();
    let steps = parsed["steps"].as_object().unwrap();
    assert!(steps["readiness-gate"]["$skill"] == json!("task-readiness-gate"));
    assert!(steps["move-qa"]["$edit"]
        .as_str()
        .unwrap()
        .contains("\"stage\""));
    assert!(steps["finish"]["$edit"]
        .as_str()
        .unwrap()
        .contains("\"append\""));
}

#[test]
fn step_line_number_points_an_editor_at_the_step() {
    let repo = Repo::empty();
    let file = init_project_pipeline(repo.path(), "task").unwrap();
    let line = step_line_number(&file, "move-qa");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.lines().nth(line - 1).unwrap().contains("\"move-qa\""));

    // An unknown step, or an unreadable file, still opens the editor.
    assert_eq!(step_line_number(&file, "no-such-step"), 1);
    assert_eq!(
        step_line_number(repo.path().join("missing.json").as_path(), "finish"),
        1
    );
}

#[test]
fn every_built_in_pipeline_can_seed_a_template() {
    for pipeline in crate::pipeline::BUILT_IN {
        let text = starter_template(pipeline);
        let parsed: Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("{} produced invalid JSON: {err}", pipeline.id));
        assert_eq!(parsed["extends"], json!(pipeline.id));
    }
}
