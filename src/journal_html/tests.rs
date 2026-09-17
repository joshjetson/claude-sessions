//! The Node app had no test for `journal-html.js` — the page was checked by
//! opening it. These pin the properties that opening it would not reveal
//! quickly: that it loads nothing over the network, that every tab and control
//! the client script wires up exists in the markup, and that the data really is
//! embedded rather than fetched.

use crate::journal::classify::SectionKind;
use crate::journal::entries::{JournalEntry, JournalSection, Rule};
use crate::journal::tasks::{LogInfo, TaskArchiveView, TranscriptInfo};
use crate::journal::{JournalData, JournalPaths, RepoSummary};
use crate::test_support::external_resource_refs;

use super::*;

fn sample() -> JournalData {
    JournalData {
        generated_at: "2026-09-16T12:00:00.000Z".into(),
        paths: JournalPaths {
            tasks_dir: "/home/dev/.claude-sessions/tasks".into(),
            prompts_dir: "/home/dev/.claude-sessions/prompts".into(),
            logs_dir: "/home/dev/.claude-sessions/logs".into(),
        },
        repos: vec![RepoSummary {
            name: "aurora".into(),
            path: "/home/dev/dev/aurora".into(),
            entries: 1,
            rules: 1,
            last_entry: Some("2026-07-23".into()),
        }],
        entries: vec![JournalEntry {
            id: "aurora:0".into(),
            repo: "aurora".into(),
            repo_path: "/home/dev/dev/aurora".into(),
            file: "/home/dev/dev/aurora/docs/agent/problem-reasoning-journal.md".into(),
            date: Some("2026-07-23".into()),
            title: "Task-5944 — Portal login redirect loop".into(),
            task_ids: vec![5944],
            sections: vec![JournalSection {
                label: "Final Fix".into(),
                body: "Moved the guard after session hydration.".into(),
                kind: SectionKind::Fix,
            }],
            body: "Moved the guard after session hydration.".into(),
            words: 6,
            is_revision: false,
            has_assumption: true,
            has_correction: true,
            has_attempts: false,
            has_rule: true,
        }],
        rules: vec![Rule {
            id: "aurora:0".into(),
            repo: "aurora".into(),
            repo_path: "/home/dev/dev/aurora".into(),
            file: "/home/dev/dev/aurora/docs/agent/project-rules.md".into(),
            section: "Auth".into(),
            text: "Auth guards must run after session middleware.".into(),
            task_ids: vec![5944],
        }],
        tasks: vec![TaskArchiveView {
            task_id: 5944,
            cwd: "/home/dev/dev/aurora".into(),
            repo: "aurora".into(),
            session_id: Some("sess-1".into()),
            session_file: Some("/x/sess-1.jsonl".into()),
            archived_at: Some("2026-07-24T10:00:00.000Z".into()),
            transcript: Some(TranscriptInfo {
                path: "/home/dev/.claude-sessions/tasks/5944/sess-1.jsonl".into(),
                bytes: 4096,
                mtime: "2026-07-24T10:00:00.000Z".into(),
            }),
            prompts: Vec::new(),
            logs: vec![LogInfo {
                date: "2026-07-24".into(),
                title: "Portal login".into(),
                summary: "Fixed the loop.".into(),
                mr_url: None,
                time: Some("05:00 PM".into()),
            }],
            entry_ids: vec!["aurora:0".into()],
        }],
    }
}

#[test]
fn the_page_loads_nothing_over_the_network() {
    let html = render_html(&sample());
    let refs = external_resource_refs(&html);
    assert!(refs.is_empty(), "the page would fetch: {refs:?}");
}

#[test]
fn the_stylesheet_and_the_client_are_inlined() {
    let html = render_html(&sample());
    assert!(html.contains("<style>"), "no inline stylesheet");
    assert!(html.contains("prefers-color-scheme"), "no dark/light pair");
    assert!(html.contains("JSON.parse(document.getElementById('data')"));
}

#[test]
fn every_tab_the_client_switches_between_is_in_the_markup() {
    let html = render_html(&sample());
    for tab in ["journal", "rules", "tasks", "repos"] {
        assert!(
            html.contains(&format!("data-tab=\"{tab}\"")),
            "missing {tab} tab"
        );
    }
    for control in [
        "id=\"q\"",
        "id=\"repo\"",
        "id=\"range\"",
        "id=\"onlyLinked\"",
        "id=\"onlyRev\"",
        "id=\"count\"",
        "id=\"list\"",
        "id=\"detail\"",
        "id=\"gen\"",
    ] {
        assert!(html.contains(control), "missing control {control}");
    }
}

#[test]
fn the_data_is_serialised_into_the_page() {
    let html = render_html(&sample());
    assert!(html.contains(r#"<script id="data" type="application/json">"#));
    assert!(html.contains("Portal login redirect loop"));
    assert!(
        html.contains("\"taskIds\":[5944]"),
        "camelCase keys the client reads"
    );
    assert!(html.contains("\"hasCorrection\":true"));
    assert!(html.contains("\"kind\":\"fix\""));
    assert!(html.contains("\"entryIds\":[\"aurora:0\"]"));
    assert!(html.contains("\"lastEntry\":\"2026-07-23\""));
}

#[test]
fn a_literal_closing_script_tag_in_an_entry_cannot_break_out_of_the_data_block() {
    let mut data = sample();
    data.entries[0].body = "before </script><script>alert(1)</script> after".into();
    let html = render_html(&data);
    let json_block = html
        .split(r#"<script id="data" type="application/json">"#)
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    assert!(
        json_block.contains("\\u003c/script"),
        "the tag survived unescaped"
    );
    assert!(
        !json_block.contains("<script>"),
        "an injected tag is in the data block"
    );
}

#[test]
fn an_empty_collection_still_renders_a_usable_page() {
    let html = render_html(&JournalData::default());
    assert!(html.contains("Problem Reasoning Journal"));
    assert!(html.contains("Select an entry."));
    assert!(external_resource_refs(&html).is_empty());
}

#[test]
fn the_summary_line_counts_what_was_collected() {
    assert_eq!(
        summary_line(&sample()),
        "1 entries · 1 rules · 1 archived tasks"
    );
}
