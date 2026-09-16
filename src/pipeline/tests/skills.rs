//! Skill discovery — how a pipeline step finds out what it can invoke.
//!
//! The Node suite scanned the machine running it (and asserted the person had
//! every pipeline skill installed, which failed on a fresh checkout). Here the
//! roots are arguments, so the walk is tested against a temp tree.

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::pipeline::{discover_skills, parse_skill_frontmatter};

fn write_skill(root: &Path, dir: &str, body: &str) -> std::path::PathBuf {
    let skill_dir = root.join(dir);
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    fs::write(&file, body).unwrap();
    file
}

#[test]
fn reads_a_plain_name_and_description() {
    let parsed = parse_skill_frontmatter(
        "---\nname: my-skill\ndescription: Does a thing.\n---\n# Body",
        "",
    );
    assert_eq!(
        parsed,
        Some(("my-skill".to_string(), "Does a thing.".to_string()))
    );
}

#[test]
fn falls_back_to_the_directory_name_when_name_is_omitted() {
    // Most installed skills have no `name` field — the directory IS the name,
    // which is how Claude Code resolves them. Without this fallback, roughly
    // two thirds of the skills on a real machine were invisible.
    let parsed =
        parse_skill_frontmatter("---\ndescription: No name field here.\n---", "odoo-review")
            .unwrap();
    assert_eq!(parsed.0, "odoo-review");
    assert_eq!(parsed.1, "No name field here.");
}

#[test]
fn reads_folded_block_descriptions() {
    let parsed = parse_skill_frontmatter(
        "---\nname: x\ndescription: >\n  first line\n  second line\n---",
        "",
    )
    .unwrap();
    assert_eq!(parsed.1, "first line second line");

    let literal =
        parse_skill_frontmatter("---\nname: x\ndescription: |\n  one\n  two\n---", "").unwrap();
    assert_eq!(literal.1, "one two");
}

#[test]
fn a_folded_block_stops_at_the_next_field() {
    let parsed = parse_skill_frontmatter(
        "---\ndescription: >\n  wrapped text\nname: after-the-block\n---",
        "dir-name",
    )
    .unwrap();
    assert_eq!(parsed.0, "after-the-block");
    assert_eq!(parsed.1, "wrapped text");
}

#[test]
fn a_file_with_no_frontmatter_still_yields_the_directory_name() {
    assert_eq!(
        parse_skill_frontmatter("# Just a heading", "fallback"),
        Some(("fallback".to_string(), String::new()))
    );
    assert_eq!(parse_skill_frontmatter("# Just a heading", ""), None);
}

#[test]
fn strips_surrounding_quotes() {
    assert_eq!(
        parse_skill_frontmatter("---\nname: \"quoted\"\n---", "")
            .unwrap()
            .0,
        "quoted"
    );
}

#[test]
fn finds_personal_plugin_and_project_local_skills() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");

    write_skill(
        &claude.join("skills"),
        "odoo-review",
        "---\ndescription: Personal.\n---\n",
    );
    write_skill(
        &claude.join("plugins"),
        "toolkit/skills/qa",
        "---\nname: qa\ndescription: From a plugin.\n---\n",
    );
    write_skill(
        &repo.path().join(".claude").join("skills"),
        "my-project-skill",
        "---\ndescription: Local to this repo.\n---\n",
    );

    let found = discover_skills(&claude, Some(repo.path()));
    let names: Vec<&str> = found.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, vec!["my-project-skill", "odoo-review", "qa"]);
    assert_eq!(
        found
            .iter()
            .find(|skill| skill.name == "my-project-skill")
            .unwrap()
            .description,
        "Local to this repo."
    );
}

#[test]
fn returns_a_sorted_deduplicated_list_with_the_newest_file_winning() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");

    // Plugins keep several versions side by side.
    write_skill(
        &claude.join("plugins").join("old"),
        "skills/qa",
        "---\nname: qa\ndescription: Old copy.\n---\n",
    );
    sleep(Duration::from_millis(20));
    write_skill(
        &claude.join("plugins").join("new"),
        "skills/qa",
        "---\nname: qa\ndescription: New copy.\n---\n",
    );

    let found = discover_skills(&claude, None);
    assert_eq!(found.len(), 1, "the same skill was listed twice");
    assert_eq!(found[0].description, "New copy.");
}

#[test]
fn hidden_directories_and_node_modules_are_skipped() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    write_skill(
        &claude.join("skills"),
        ".hidden-skill",
        "---\nname: hidden\n---\n",
    );
    write_skill(
        &claude.join("skills").join("node_modules"),
        "packaged",
        "---\nname: packaged\n---\n",
    );
    write_skill(&claude.join("skills"), "real", "---\nname: real\n---\n");

    let names: Vec<String> = discover_skills(&claude, None)
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, vec!["real".to_string()]);
}

#[test]
fn the_walk_stops_before_it_wanders_off() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    // Seven levels below the root — past the depth limit.
    write_skill(
        &claude.join("plugins"),
        "a/b/c/d/e/f/g/too-deep",
        "---\nname: too-deep\n---\n",
    );
    assert!(discover_skills(&claude, None).is_empty());
}

#[test]
fn missing_roots_are_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    assert!(discover_skills(&home.path().join("nothing-here"), None).is_empty());
}
