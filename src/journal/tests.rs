//! Ported from the Node app's `test/journal.test.js`, plus the discovery and
//! join cases that suite never covered (they needed a real home directory).

use std::fs;
use std::path::PathBuf;

use super::*;

/// A throwaway repo with `docs/agent/*.md` in it.
pub(super) fn repo_with(
    root: &Path,
    name: &str,
    journal: Option<&str>,
    rules: Option<&str>,
) -> PathBuf {
    let repo = root.join(name);
    fs::create_dir_all(repo.join(AGENT_DOCS_SUBDIR)).unwrap();
    if let Some(journal) = journal {
        fs::write(journal_path(&repo), journal).unwrap();
    }
    if let Some(rules) = rules {
        fs::write(rules_path(&repo), rules).unwrap();
    }
    repo
}

pub(super) const FULL_FORM: &str = r#"# Problem Reasoning Journal

## [2026-07-23] Task-5944 — Portal login redirect loop

### Problem
Users bounced between /login and /dashboard forever.

### Initial Assumption
I assumed the session cookie was expiring early.

### Investigation
The cookie was fine; the redirect guard ran before the session middleware.

### User Correction
The user pointed out it only happened behind the proxy.

### Final Fix
Moved the guard to run after session hydration.

### Reusable Rule
Auth guards must run after session middleware, never before.

## [2026-07-30] Task 6117 REVISION — Invoice totals off by a cent

### Problem
Rounding drift on multi-line invoices.

### Final Fix
Round once at the total, not per line.
"#;

pub(super) const COMPACT_FORM: &str = r#"# Problem Reasoning Journal

## [2026-08-02] Task-5699 — Scraper dropped rows silently

**Root cause:** the upstream feed switched to gzip and the reader kept the old branch.

**Fix:** detect the content-encoding header instead of assuming identity.

Extra prose continuing the fix section.

**Reusable rule:** never assume a transfer encoding — read the header.
"#;

pub(super) const RULES: &str = r#"# Project Rules

## Auth

- Auth guards must run after session middleware.
  Continuation line for the same rule.
- Never trust a proxy-forwarded header without validation. See Task-5944.

## Data

* Round money once, at the total (task 6117).
"#;

fn kinds(entry: &JournalEntry) -> std::collections::HashMap<SectionKind, String> {
    entry
        .sections
        .iter()
        .map(|s| (s.kind, s.body.clone()))
        .collect()
}

// --- parse_journal ----------------------------------------------------------

#[test]
fn splits_entries_and_pulls_out_date_title_and_task_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(FULL_FORM), None));
    assert_eq!(entries.len(), 2);

    assert_eq!(entries[0].date.as_deref(), Some("2026-07-23"));
    assert_eq!(entries[0].title, "Task-5944 — Portal login redirect loop");
    assert_eq!(entries[0].task_ids, vec![5944]);

    assert_eq!(entries[1].date.as_deref(), Some("2026-07-30"));
    assert_eq!(
        entries[1].task_ids,
        vec![6117],
        "space-separated \"Task 6117\" must be recognised"
    );
}

#[test]
fn classifies_the_heading_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(FULL_FORM), None));
    let k = kinds(&entries[0]);
    assert!(k[&SectionKind::Problem].starts_with("Users bounced"));
    assert!(k[&SectionKind::Assumption].contains("session cookie"));
    assert!(k[&SectionKind::Correction].contains("behind the proxy"));
    assert!(k[&SectionKind::Fix].contains("after session hydration"));
    assert!(k[&SectionKind::Rule].contains("after session middleware"));
    assert!(k[&SectionKind::Investigation].contains("redirect guard"));
}

#[test]
fn derives_the_reasoning_flags_used_for_filtering() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(FULL_FORM), None));
    let (full, revision) = (&entries[0], &entries[1]);
    assert!(full.has_assumption);
    assert!(full.has_correction);
    assert!(full.has_rule);
    assert!(!full.is_revision);
    assert!(
        revision.is_revision,
        "\"REVISION\" in the title sets the flag"
    );
    assert!(!revision.has_rule);
}

#[test]
fn parses_the_compact_bold_entry_form_as_well_as_the_heading_form() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = parse_journal(&repo_with(tmp.path(), "scraper", Some(COMPACT_FORM), None));
    let k = kinds(&entries[0]);
    assert!(k[&SectionKind::RootCause].contains("gzip"));
    assert!(k[&SectionKind::Fix].contains("content-encoding"));
    assert!(
        k[&SectionKind::Fix].contains("Extra prose"),
        "trailing prose must attach to the open section"
    );
    assert!(k[&SectionKind::Rule].contains("read the header"));
    assert_eq!(entries[0].task_ids, vec![5699]);
}

#[test]
fn detects_corrections_written_as_prose_without_a_heading() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "# J\n\n## [2026-08-03] Task-1234 — Thing\n\n\
        **Fix:** something. QA correctly flagged the missing case.\n";
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(md), None));
    assert!(entries[0].has_correction);

    let md = "# J\n\n## [2026-08-03] Task-1235 — Thing\n\n\
        **Fix:** the user pointed me to the other adapter.\n";
    let entries = parse_journal(&repo_with(tmp.path(), "app2", Some(md), None));
    assert!(entries[0].has_correction);
}

#[test]
fn detects_assumptions_written_as_prose() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "# J\n\n## [2026-08-03] Task-1236 — Thing\n\n\
        **Fix:** I assumed the cache was warm; it was not.\n";
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(md), None));
    assert!(entries[0].has_assumption);
}

#[test]
fn a_reverted_title_is_not_a_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "# J\n\n## [2026-08-03] Task-1237 — Reverted the migration\n\nBody.\n";
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(md), None));
    assert!(!entries[0].is_revision, "\"Reverted\" is not \"revision\"");
}

#[test]
fn an_entry_with_no_date_still_parses_keeping_the_whole_heading_as_title() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "# J\n\n## Untitled investigation\n\nSome body.\n";
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(md), None));
    assert_eq!(entries[0].date, None);
    assert_eq!(entries[0].title, "Untitled investigation");
    assert!(entries[0].task_ids.is_empty());
}

#[test]
fn a_missing_or_empty_journal_yields_no_entries_instead_of_failing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(parse_journal(&repo_with(tmp.path(), "bare", None, None)).is_empty());
    assert!(parse_journal(Path::new("/nonexistent/repo/path")).is_empty());
    assert!(parse_journal(&repo_with(
        tmp.path(),
        "preamble",
        Some("# Problem Reasoning Journal\n\nNothing yet.\n"),
        None
    ))
    .is_empty());
}

#[test]
fn entry_ids_are_stable_and_unique_within_a_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with(tmp.path(), "app", Some(FULL_FORM), None);
    let first = parse_journal(&repo);
    let again = parse_journal(&repo);
    let ids: Vec<&str> = first.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        first.len()
    );
    assert_eq!(ids, again.iter().map(|e| e.id.as_str()).collect::<Vec<_>>());
    assert_eq!(first[0].id, "app:0");
}

#[test]
fn a_word_count_and_the_body_come_back_with_the_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = parse_journal(&repo_with(tmp.path(), "app", Some(COMPACT_FORM), None));
    assert!(entries[0].words > 20, "{}", entries[0].words);
    assert!(entries[0].body.contains("gzip"));
    assert!(entries[0].file.ends_with("problem-reasoning-journal.md"));
}

// --- parse_rules ------------------------------------------------------------

#[test]
fn collects_bullets_tracks_their_section_and_folds_continuation_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = parse_rules(&repo_with(tmp.path(), "app", None, Some(RULES)));
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].section, "Auth");
    assert!(rules[0].text.contains("Continuation line"));
    assert_eq!(rules[2].section, "Data");
    assert!(
        rules[2].text.starts_with("Round money once"),
        "both - and * bullets must be recognised"
    );
}

#[test]
fn links_rules_back_to_task_ids_mentioned_in_their_text() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = parse_rules(&repo_with(tmp.path(), "app", None, Some(RULES)));
    assert_eq!(rules[1].task_ids, vec![5944]);
    assert_eq!(rules[2].task_ids, vec![6117]);
    assert!(rules[0].task_ids.is_empty());
}

#[test]
fn a_missing_rules_file_yields_no_rules_instead_of_failing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(parse_rules(&repo_with(tmp.path(), "bare", None, None)).is_empty());
    assert!(parse_rules(Path::new("/nonexistent/repo")).is_empty());
}

#[test]
fn rule_ids_are_unique_within_a_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = parse_rules(&repo_with(tmp.path(), "app", None, Some(RULES)));
    let ids: std::collections::HashSet<&str> = rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids.len(), rules.len());
}

// --- task ids ---------------------------------------------------------------

#[test]
fn every_spelling_of_a_task_reference_is_recognised() {
    use super::classify::extract_task_ids;
    assert_eq!(extract_task_ids("Task-5944"), vec![5944]);
    assert_eq!(extract_task_ids("Task 6117"), vec![6117]);
    assert_eq!(extract_task_ids("task-5699"), vec![5699]);
    assert_eq!(extract_task_ids("#5882"), vec![5882]);
    assert_eq!(extract_task_ids("task #4033"), vec![4033]);
    // Three digits is the floor for "task N", four for a bare "#N".
    assert_eq!(extract_task_ids("task 42"), Vec::<i64>::new());
    assert_eq!(extract_task_ids("#42"), Vec::<i64>::new());
    // Seven digits is not a task id.
    assert_eq!(extract_task_ids("task 12345678"), Vec::<i64>::new());
    // The word boundary before "task" is load-bearing: a word that merely ends
    // in "task" is not a reference, which is Node's behaviour and keeps
    // "subtask-5944" out of the parent task's entry list.
    assert_eq!(extract_task_ids("subtask-5944"), Vec::<i64>::new());
    assert_eq!(extract_task_ids("multitasking 5944"), Vec::<i64>::new());
    // First-seen order, no duplicates.
    assert_eq!(
        extract_task_ids("Task-1234 and #1234 and task 5678"),
        vec![1234, 5678]
    );
}

/// A config handle over a literal config file.
pub(super) fn config_for(paths: &Paths, json: &str) -> crate::config::ConfigHandle {
    fs::write(&paths.config_path, json).unwrap();
    crate::config::ConfigHandle::load(paths, crate::config::EnvOverrides::default())
}

mod discovery;
