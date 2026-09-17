//! Repo discovery and the join between the journals and the task archive.
//!
//! The Node suite could not cover these — they needed a real home directory —
//! so these are new: the sibling-worktree widening, the group-folder sweep, and
//! the entry-to-archive join the viewer is built on.

use std::fs;

use super::super::*;
use super::{config_for, repo_with, COMPACT_FORM, FULL_FORM, RULES};

#[test]
fn repos_are_discovered_from_config_and_widened_into_sibling_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let dev = tmp.path().join("dev");
    // The configured repo, and a sibling worktree beside it that config has
    // never heard of — the case the widening exists for.
    let main = repo_with(&dev, "aurora", Some(FULL_FORM), None);
    let worktree = repo_with(&dev, "aurora-task-5699", Some(COMPACT_FORM), None);
    // A sibling with no docs/agent is not a journal repo.
    fs::create_dir_all(dev.join("unrelated")).unwrap();

    let config = config_for(
        &paths,
        &format!(
            r#"{{"odooProjectDirs": {{"Aurora": "{}"}}}}"#,
            main.display()
        ),
    );
    let found = discover_repos(&paths, &config);
    assert!(found.contains(&main), "{found:?}");
    assert!(
        found.contains(&worktree),
        "sibling worktree missed: {found:?}"
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn a_group_directorys_children_are_candidates_too() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let group = tmp.path().join("clients");
    let repo = repo_with(&group, "novalink", Some(FULL_FORM), None);
    let config = config_for(
        &paths,
        &format!(
            r#"{{"groups": [{{"name": "Clients", "path": "{}"}}]}}"#,
            group.display()
        ),
    );
    assert_eq!(discover_repos(&paths, &config), vec![repo]);
}

#[test]
fn an_archived_tasks_cwd_seeds_discovery_even_without_config() {
    // The other half of the widening: a worktree nobody configured, found
    // because a finished task was archived out of it.
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let repo = repo_with(
        &tmp.path().join("scratch"),
        "orbit",
        Some(COMPACT_FORM),
        None,
    );
    fs::create_dir_all(paths.task_dir(5699)).unwrap();
    fs::write(
        paths.task_dir(5699).join("meta.json"),
        format!(r#"{{"taskId": 5699, "cwd": "{}"}}"#, repo.display()),
    )
    .unwrap();

    let config = config_for(&paths, "{}");
    assert_eq!(discover_repos(&paths, &config), vec![repo]);
}

#[test]
fn collect_joins_entries_to_the_task_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let dev = tmp.path().join("dev");
    let repo = repo_with(&dev, "aurora", Some(FULL_FORM), Some(RULES));

    // An archive for one of the two tasks the journal names.
    let task_dir = paths.task_dir(5944);
    fs::create_dir_all(&task_dir).unwrap();
    fs::write(
        task_dir.join("meta.json"),
        format!(
            r#"{{"taskId": 5944, "cwd": "{}", "sessionId": "sess-1",
                 "sessionFile": "/x/sess-1.jsonl", "archivedAt": "2026-07-24T10:00:00.000Z"}}"#,
            repo.display()
        ),
    )
    .unwrap();
    fs::write(task_dir.join("sess-1.jsonl"), "{}\n").unwrap();
    fs::create_dir_all(&paths.prompts_dir).unwrap();
    fs::write(
        paths.prompts_dir.join("task-5944-1756800000000.txt"),
        "prompt",
    )
    .unwrap();
    fs::create_dir_all(&paths.logs_dir).unwrap();
    fs::write(
        paths.logs_dir.join("2026-07-24.md"),
        "# Daily log — 2026-07-24\n\n\
         - **#5944 Portal login** — Fixed the loop properly.  _(05:00 PM)_\n",
    )
    .unwrap();

    let config = config_for(
        &paths,
        &format!(
            r#"{{"odooProjectDirs": {{"Aurora": "{}"}}}}"#,
            repo.display()
        ),
    );
    let data = collect(&paths, &config, None);

    assert_eq!(data.repos.len(), 1);
    assert_eq!(data.repos[0].name, "aurora");
    assert_eq!(data.repos[0].entries, 2);
    assert_eq!(data.repos[0].rules, 3);
    assert_eq!(data.repos[0].last_entry.as_deref(), Some("2026-07-30"));

    // Newest entry first.
    assert_eq!(data.entries[0].date.as_deref(), Some("2026-07-30"));

    let task = data.tasks.iter().find(|t| t.task_id == 5944).unwrap();
    assert_eq!(task.repo, "aurora");
    assert_eq!(task.session_id.as_deref(), Some("sess-1"));
    assert!(task.transcript.is_some());
    assert_eq!(task.prompts.len(), 1);
    assert_eq!(task.logs.len(), 1);
    assert_eq!(task.logs[0].summary, "Fixed the loop properly.");
    // The 2026-07-23 entry is the one naming 5944; ids follow file order, not
    // the newest-first display order.
    assert_eq!(task.entry_ids, vec!["aurora:0".to_string()]);
    assert_eq!(data.linked_tasks(), 1);
    assert!(task
        .resume_command()
        .unwrap()
        .contains("claude --resume sess-1"));
    assert!(!data.generated_at.is_empty());
}

#[test]
fn collect_on_an_empty_home_is_empty_rather_than_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let config = config_for(&paths, "{}");
    let data = collect(&paths, &config, None);
    assert!(data.repos.is_empty());
    assert!(data.entries.is_empty());
    assert!(data.tasks.is_empty());
}

#[test]
fn an_archive_without_a_readable_meta_is_skipped_not_invented() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    fs::create_dir_all(paths.task_dir(1)).unwrap();
    fs::write(paths.task_dir(1).join("meta.json"), "{ nope").unwrap();
    fs::create_dir_all(paths.tasks_dir.join("not-a-task")).unwrap();
    assert!(tasks::list_task_metas(&paths).is_empty());
}
