//! One level of a group directory.

use std::fs;

use crate::scan::discover_projects;

#[test]
fn lists_subdirectories_sorted_skipping_dotfiles_and_the_ignored_set() {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["zebra", "apple", ".hidden", "node_modules", ".venv", ".git"] {
        fs::create_dir(dir.path().join(name)).expect("mkdir");
    }
    fs::write(dir.path().join("README.md"), "x").expect("write");

    assert_eq!(discover_projects(dir.path()), vec!["apple", "zebra"]);
}

#[test]
fn an_unreadable_group_is_empty_not_an_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(discover_projects(&dir.path().join("not-mounted")).is_empty());
}

#[test]
fn discovery_does_not_descend() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("repo/src/inner")).expect("mkdir");
    assert_eq!(discover_projects(dir.path()), vec!["repo"]);
}
