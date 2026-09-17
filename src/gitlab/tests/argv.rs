//! The pure builders: URL encoding, query strings, flag combinations, and the
//! merge-request URL parser.

use std::path::Path;

use crate::gitlab::argv::*;

#[test]
fn a_project_path_is_encoded_as_one_segment() {
    // The load-bearing case: every slash must become %2F or the request
    // addresses a route that is not there.
    assert_eq!(encode_uri_component("group/sub/repo"), "group%2Fsub%2Frepo");
    assert_eq!(
        mr_api_path("group/sub/repo", 403),
        "projects/group%2Fsub%2Frepo/merge_requests/403"
    );
}

#[test]
fn encoding_leaves_the_unreserved_characters_alone_and_escapes_the_rest() {
    assert_eq!(
        encode_uri_component("a-b_c.d!e~f*g'h(i)"),
        "a-b_c.d!e~f*g'h(i)"
    );
    assert_eq!(encode_uri_component("a b&c=d?e#f"), "a%20b%26c%3Dd%3Fe%23f");
    // Multi-byte input is encoded per UTF-8 byte, as encodeURIComponent does.
    assert_eq!(encode_uri_component("é"), "%C3%A9");
}

#[test]
fn the_open_mrs_query_is_the_one_the_node_wrapper_sent() {
    assert_eq!(
        open_mrs_api_path(MrScope::CreatedByMe),
        "merge_requests?state=opened&scope=created_by_me&order_by=updated_at&per_page=100"
    );
    assert_eq!(
        open_mrs_api_path(MrScope::AssignedToMe),
        "merge_requests?state=opened&scope=assigned_to_me&order_by=updated_at&per_page=100"
    );
    assert_eq!(
        api_args("merge_requests?state=opened"),
        vec!["api", "merge_requests?state=opened"]
    );
}

#[test]
fn merge_flags_are_opt_in_and_ordered() {
    let plain = merge_args("group/repo", 12, MergeOptions::default());
    assert_eq!(
        plain,
        vec!["mr", "merge", "12", "--repo", "group/repo", "--yes"]
    );

    let both = merge_args(
        "group/repo",
        12,
        MergeOptions {
            squash: true,
            remove_source_branch: true,
        },
    );
    assert_eq!(&both[6..], ["--squash", "--remove-source-branch"]);

    let squash_only = merge_args(
        "group/repo",
        12,
        MergeOptions {
            squash: true,
            ..MergeOptions::default()
        },
    );
    assert_eq!(&squash_only[6..], ["--squash"]);

    let remove_only = merge_args(
        "group/repo",
        12,
        MergeOptions {
            remove_source_branch: true,
            ..MergeOptions::default()
        },
    );
    assert_eq!(&remove_only[6..], ["--remove-source-branch"]);
}

#[test]
fn the_repo_is_passed_by_path_not_by_working_directory() {
    // The Deploy tab merges MRs in repositories this machine has never cloned.
    let args = merge_args("group/sub/repo", 7, MergeOptions::default());
    let at = args.iter().position(|a| a == "--repo").unwrap();
    assert_eq!(args[at + 1], "group/sub/repo", "not URL-encoded for --repo");
}

#[test]
fn the_git_commands_name_their_repository_with_dash_c() {
    let cwd = Path::new("/Users/dev/orbit media");
    assert_eq!(
        current_branch_args(cwd),
        vec![
            "-C",
            "/Users/dev/orbit media",
            "rev-parse",
            "--abbrev-ref",
            "HEAD"
        ]
    );
    assert_eq!(
        push_args(cwd, "task-4242-widget"),
        vec![
            "-C",
            "/Users/dev/orbit media",
            "push",
            "-u",
            "origin",
            "task-4242-widget",
        ]
    );
}

#[test]
fn creating_a_merge_request_fills_itself_in_and_never_asks() {
    assert_eq!(
        create_mr_args("task-4242-widget", "development"),
        vec![
            "mr",
            "create",
            "--fill",
            "--yes",
            "--source-branch",
            "task-4242-widget",
            "--target-branch",
            "development",
        ]
    );
}

#[test]
fn a_merge_request_is_never_opened_from_a_protected_or_detached_branch() {
    assert!(can_open_mr_from("task-4242-widget"));
    for branch in ["development", "main", "master", "HEAD", ""] {
        assert!(!can_open_mr_from(branch), "{branch} should be refused");
    }
}

#[test]
fn the_merge_request_url_parser_survives_subgroups_and_trailing_paths() {
    let parsed = parse_mr_url("https://git.example.com/group/sub/repo/-/merge_requests/403");
    assert_eq!(
        parsed,
        Some(MrRef {
            project_path: "group/sub/repo".to_string(),
            iid: 403,
        })
    );
    // A diff or note link still names the merge request.
    assert_eq!(
        parse_mr_url("http://git.example.com/group/repo/-/merge_requests/12/diffs#note_9")
            .map(|r| r.iid),
        Some(12)
    );
}

#[test]
fn anything_that_is_not_a_merge_request_url_parses_to_nothing() {
    for url in [
        "",
        "not a url",
        "https://git.example.com/group/repo/-/issues/403",
        "https://git.example.com/-/merge_requests/403",
        "https://git.example.com/group/repo/-/merge_requests/notanumber",
        "ftp://git.example.com/group/repo/-/merge_requests/1",
    ] {
        assert_eq!(parse_mr_url(url), None, "{url} should not parse");
    }
}

#[test]
fn a_listing_row_names_its_project_from_the_reference_then_the_url() {
    assert_eq!(
        project_from_ref(Some("group/sub/repo!403"), ""),
        "group/sub/repo"
    );
    assert_eq!(
        project_from_ref(
            None,
            "https://git.example.com/group/repo/-/merge_requests/9"
        ),
        "group/repo"
    );
    assert_eq!(project_from_ref(None, ""), "");
}
