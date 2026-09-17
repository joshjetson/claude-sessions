//! The Node app checked this page by opening it. These pin what opening it
//! would not: that it fetches nothing, that every built-in pipeline is on it,
//! and that a project override is reported only when it actually changes
//! something.

use std::fs;

use crate::test_support::external_resource_refs;

use super::*;

#[test]
fn the_page_loads_nothing_over_the_network() {
    let html = render_html(&[]);
    let refs = external_resource_refs(&html);
    assert!(refs.is_empty(), "the page would fetch: {refs:?}");
    assert!(html.contains("<style>"), "the stylesheet must be inlined");
    assert!(!html.contains("<script"), "the page needs no script at all");
}

#[test]
fn every_built_in_pipeline_has_a_section() {
    let html = render_html(&[]);
    for pipeline in BUILT_IN {
        let resolved = resolve_pipeline(pipeline.id, None).unwrap();
        assert!(
            html.contains(&esc(&resolved.name)),
            "missing the {} pipeline",
            pipeline.id
        );
    }
    assert_eq!(
        html.matches(r#"<section class="pipeline">"#).count(),
        BUILT_IN.len(),
        "one section per built-in pipeline"
    );
}

#[test]
fn a_step_shows_its_prompt_text_and_its_badges() {
    let html = render_html(&[]);
    assert!(html.contains("<summary>prompt text</summary>"));
    assert!(html.contains("the complete prompt this sends"));
    assert!(html.contains(r#"<span class="badge gate">can stop here</span>"#));
    assert!(html.contains(r#"<span class="badge dash">the dashboard does this</span>"#));
}

#[test]
fn the_sample_prompt_carries_no_real_host_or_absolute_temp_path() {
    // The page is generated on a work machine and shown to anyone.
    let html = render_html(&[]);
    assert!(
        html.contains("odoo.example.com"),
        "the sample URL is a placeholder"
    );
    assert!(
        !html.contains("/tmp/claude-sessions-task-"),
        "the Node original baked an absolute /tmp path into every prompt"
    );
}

#[test]
fn the_sample_vars_are_obviously_examples() {
    let vars = default_sample_vars();
    assert_eq!(vars.task_id, 5944);
    assert!(vars.url.contains("example.com"));
    assert!(vars.summary_file.starts_with("~/.claude-sessions/"));
    assert_eq!(vars.target_branch.as_deref(), Some("development"));

    let other = sample_vars(7, "main");
    assert!(other.branch_instruction.contains("`main`"));
    assert!(other.url.contains("id=7"));
}

#[test]
fn a_repo_with_no_override_is_not_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let html = render_html(&[ProjectRepo {
        name: "Aurora".into(),
        path: tmp.path().display().to_string(),
    }]);
    assert!(
        html.contains("No project overrides yet."),
        "{}",
        &html[..200]
    );
}

#[test]
fn a_repo_whose_override_changes_nothing_is_not_listed_either() {
    // The starter template is inert until it is edited; claiming it as a
    // customisation would make every initialised repo look changed.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude-sessions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pipeline.json"), r#"{"extends": "task"}"#).unwrap();
    let html = render_html(&[ProjectRepo {
        name: "Aurora".into(),
        path: tmp.path().display().to_string(),
    }]);
    assert!(html.contains("No project overrides yet."));
}

#[test]
fn a_repo_that_really_changed_a_step_gets_a_section_and_a_nav_link() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude-sessions");
    fs::create_dir_all(&dir).unwrap();
    let first_step = BUILT_IN
        .iter()
        .find(|p| p.id == "task")
        .unwrap()
        .steps
        .first()
        .unwrap()
        .id;
    fs::write(
        dir.join("pipeline.json"),
        format!(r#"{{"extends": "task", "steps": {{"{first_step}": {{"skip": true}}}}}}"#),
    )
    .unwrap();

    let html = render_html(&[ProjectRepo {
        name: "Orbit Media".into(),
        path: tmp.path().display().to_string(),
    }]);
    assert!(!html.contains("No project overrides yet."));
    assert!(html.contains(r##"href="#p-Orbit-Media""##), "no nav anchor");
    assert!(
        html.contains(r#"<div id="p-Orbit-Media">"#),
        "no section anchor"
    );
    assert!(html.contains("1 changed"));
    assert!(html.contains(r#"<span class="badge skip">skipped</span>"#));
    assert!(
        html.contains("pipeline.json"),
        "the override's path is shown"
    );
}

#[test]
fn html_special_characters_in_a_project_name_are_escaped() {
    let tmp = tempfile::tempdir().unwrap();
    let html = render_html(&[ProjectRepo {
        name: "<script>".into(),
        path: tmp.path().display().to_string(),
    }]);
    assert!(
        !html.contains("<script>"),
        "an unescaped tag reached the page"
    );
}
