//! Comparing and taking apart directory paths.
//!
//! Four features used to do this inline with `trim_end_matches('/')`,
//! `starts_with(format!("{dir}/"))` and `rsplit('/')`, which is right on Unix
//! and blind on Windows: a group never claimed a session, a launch guessed the
//! whole path as a folder name, and a queued launch never matched the session
//! it started. The Windows spellings below are asserted on every platform —
//! `cfg!(windows)` picks the expected answer rather than skipping the case — so
//! the rule is visible here even when the compiler is not building for it.

use crate::util::{
    child_dir_of, is_within_dir, join_dir, path_leaf, same_dir, trim_trailing_separators,
    SEPARATORS,
};

/// A backslash is a separator only where it is one: on Unix it is a legal
/// character in a directory name, and splitting on it would rename somebody's
/// folder out from under them.
const BACKSLASH_IS_A_SEPARATOR: bool = cfg!(windows);

#[test]
fn the_separator_set_is_the_platforms() {
    let expected: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    assert_eq!(SEPARATORS, expected);
}

#[test]
fn trailing_separators_never_change_which_directory_is_meant() {
    assert_eq!(trim_trailing_separators("/Users/k/dev"), "/Users/k/dev");
    assert_eq!(trim_trailing_separators("/Users/k/dev/"), "/Users/k/dev");
    assert_eq!(trim_trailing_separators("/Users/k/dev///"), "/Users/k/dev");
    // Every caller reads the empty string as "no directory given", which is the
    // right answer for a path that was nothing but separators.
    assert_eq!(trim_trailing_separators("/"), "");
    assert_eq!(trim_trailing_separators(""), "");
}

#[test]
fn a_windows_path_is_trimmed_only_where_a_backslash_separates() {
    let trimmed = trim_trailing_separators(r"C:\src\repo\");
    if BACKSLASH_IS_A_SEPARATOR {
        assert_eq!(trimmed, r"C:\src\repo");
    } else {
        assert_eq!(trimmed, r"C:\src\repo\");
    }
}

#[test]
fn two_spellings_of_one_directory_compare_equal() {
    assert!(same_dir("/Users/k/dev", "/Users/k/dev/"));
    assert!(same_dir("/Users/k/dev//", "/Users/k/dev"));
    assert!(!same_dir("/Users/k/dev", "/Users/k/devs"));
    assert!(!same_dir("/Users/k/dev", ""));
}

#[test]
fn case_folds_only_where_the_filesystem_folds_it() {
    // A group typed into the config as `C:\Src` against a working directory a
    // transcript spelled `c:\src` is one folder on Windows and two on Unix.
    assert_eq!(same_dir(r"C:\Src", r"c:\src"), cfg!(windows));
    assert_eq!(same_dir("/Users/K/Dev", "/users/k/dev"), cfg!(windows));
}

#[test]
fn a_directory_contains_itself_and_everything_under_it() {
    assert!(is_within_dir("/Users/k/dev", "/Users/k/dev"));
    assert!(is_within_dir("/Users/k/dev/", "/Users/k/dev"));
    assert!(is_within_dir("/Users/k/dev/app", "/Users/k/dev"));
    assert!(is_within_dir("/Users/k/dev/app/sub", "/Users/k/dev/"));
    // A shared prefix is not containment: `dev-worktrees` is not in `dev`.
    assert!(!is_within_dir("/Users/k/dev-worktrees/app", "/Users/k/dev"));
    assert!(!is_within_dir("/Users/other/app", "/Users/k/dev"));
    assert!(!is_within_dir("/Users/k/dev", ""));
}

#[test]
fn a_group_claims_the_checkout_a_session_is_running_in() {
    assert_eq!(child_dir_of("/src/repo/app", "/src"), Some("repo"));
    assert_eq!(child_dir_of("/src/repo", "/src"), Some("repo"));
    assert_eq!(child_dir_of("/src/repo", "/src///"), Some("repo"));
    assert_eq!(child_dir_of("/src/repo/", "/src"), Some("repo"));
    // The group directory itself is not a checkout inside the group, and a
    // sibling that merely starts with the same letters is not in it at all.
    assert_eq!(child_dir_of("/src", "/src"), None);
    assert_eq!(child_dir_of("/srcs/repo", "/src"), None);
    assert_eq!(child_dir_of("/other/repo", "/src"), None);
    assert_eq!(child_dir_of("/src/repo", ""), None);
}

#[test]
fn a_windows_session_is_claimed_by_a_windows_group() {
    // The bug this is here for: `format!("{group}/")` against `C:\src\repo`
    // matched nothing, so every Windows session fell through to "Other
    // sessions" and the group rendered empty.
    let claimed = child_dir_of(r"C:\src\repo\app", r"C:\src");
    assert_eq!(claimed, BACKSLASH_IS_A_SEPARATOR.then_some("repo"));
    // A group path written with forward slashes — which is what `~/dev`
    // expands to on a machine whose home is `C:\Users\k` — still matches a
    // backslash-spelled working directory.
    let mixed = child_dir_of(r"C:\Users\k\dev\repo", "C:/Users/k/dev");
    assert_eq!(mixed, BACKSLASH_IS_A_SEPARATOR.then_some("repo"));
    // And a UNC path is tolerated as the string it is.
    let unc = child_dir_of(r"\\build\share\repo\app", r"\\build\share");
    assert_eq!(unc, BACKSLASH_IS_A_SEPARATOR.then_some("repo"));
}

#[test]
fn the_leaf_is_the_last_segment_whatever_the_path_ends_with() {
    assert_eq!(path_leaf("/Users/k/dev/app"), "app");
    assert_eq!(path_leaf("/Users/k/dev/app/"), "app");
    assert_eq!(path_leaf("/app"), "app");
    assert_eq!(path_leaf("app"), "app");
    assert_eq!(path_leaf(""), "");
    assert_eq!(path_leaf("/"), "");
}

#[test]
fn a_windows_leaf_is_the_segment_after_the_last_backslash() {
    // `rsplit('/')` answered `C:\src\repo` with the whole path, which is what
    // a launched terminal was titled and what the directory guess compared.
    let expected = if BACKSLASH_IS_A_SEPARATOR {
        "repo"
    } else {
        r"C:\src\repo"
    };
    assert_eq!(path_leaf(r"C:\src\repo"), expected);
    // …and on Unix a trailing backslash is part of the name, not a separator
    // to trim, so the whole string comes back as written.
    let trailing = if BACKSLASH_IS_A_SEPARATOR {
        "repo"
    } else {
        r"C:\src\repo\"
    };
    assert_eq!(path_leaf(r"C:\src\repo\"), trailing);
}

#[test]
fn joining_keeps_the_spelling_the_base_already_uses() {
    assert_eq!(join_dir("/src", "repo"), "/src/repo");
    assert_eq!(join_dir("/src/", "repo"), "/src/repo");
    assert_eq!(join_dir("/src///", "repo"), "/src/repo");
    assert_eq!(join_dir("", "repo"), "repo");
    // `/` is the one base that keeps its separator, because trimming it away
    // leaves nothing to join to.
    assert_eq!(join_dir("/", "repo"), "/repo");
    // A base with no separator in it at all takes the platform's.
    let sep = if cfg!(windows) { '\\' } else { '/' };
    assert_eq!(join_dir("src", "repo"), format!("src{sep}repo"));
}

#[test]
fn a_windows_base_grows_a_windows_child() {
    let joined = join_dir(r"C:\src", "repo");
    if BACKSLASH_IS_A_SEPARATOR {
        assert_eq!(joined, r"C:\src\repo");
    } else {
        // On Unix there is no separator in that string to copy, so the
        // platform's own is used and the result is the nonsense the input was.
        assert_eq!(joined, r"C:\src/repo");
    }
}

#[test]
fn a_drive_root_stays_a_drive_root_when_something_is_joined_to_it() {
    // `C:` alone is drive-RELATIVE on Windows — `C:repo` means "repo in the
    // current directory of drive C", which is not what a group called `C:\`
    // meant.
    let joined = join_dir(r"C:\", "repo");
    if BACKSLASH_IS_A_SEPARATOR {
        assert_eq!(joined, r"C:\repo");
    } else {
        assert_eq!(joined, r"C:\/repo");
    }
}
