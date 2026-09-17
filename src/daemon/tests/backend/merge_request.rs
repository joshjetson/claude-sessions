//! The merge-request safety net: when a finished task gets one opened for it,
//! and — more importantly — the three cases where it must not.

use super::{backend, backend_with, gitlab_fields, mr_request};
use crate::daemon::{resolve_target_branch, MergeRequestRequest, TaskBackend};
use crate::gitlab::tests::{failed, ok};
use crate::odoo::tests::stub::Reply;
use serde_json::json;

// --- the merge-request safety net -------------------------------------------

#[test]
fn a_task_that_already_has_a_merge_request_gets_no_second_one() {
    // A duplicate MR against the same branch is noise somebody has to close.
    let url = "https://git.example.com/group/repo/-/merge_requests/403";
    let (backend, _server, runner) = backend_with(vec![gitlab_fields(url)], Vec::new());
    assert_eq!(
        backend.ensure_merge_request(&mr_request()).unwrap(),
        Some(url.to_string())
    );
    assert_eq!(runner.call_count(), 0, "git was never run");
}

#[test]
fn no_merge_request_is_opened_from_a_protected_branch_or_a_detached_head() {
    // These are where merge requests LAND. Opening one from them means
    // something upstream went wrong, and an MR would not fix it.
    for branch in ["development", "main", "master", "HEAD"] {
        let (backend, _server, runner) = backend_with(
            vec![gitlab_fields("")],
            vec![
                ok(branch),
                ok(""),
                ok("https://git.example.com/x/-/merge_requests/1"),
            ],
        );
        assert_eq!(
            backend.ensure_merge_request(&mr_request()).unwrap(),
            None,
            "{branch} must not open a merge request"
        );
        assert_eq!(
            runner.call_count(),
            1,
            "{branch}: only rev-parse should have run"
        );
    }
}

#[test]
fn a_task_with_no_working_directory_is_left_alone() {
    let (backend, server, runner) = backend_with(vec![gitlab_fields("")], Vec::new());
    let request = MergeRequestRequest {
        cwd: "".into(),
        ..mr_request()
    };
    assert_eq!(backend.ensure_merge_request(&request).unwrap(), None);
    assert_eq!(runner.call_count(), 0);
    assert!(server.calls().is_empty(), "Odoo was not even asked");
}

#[test]
fn a_feature_branch_is_pushed_and_a_merge_request_opened_against_the_target() {
    let (backend, _server, runner) = backend_with(
        vec![
            gitlab_fields(""),
            // project.project read for the linked repository…
            Reply::result(json!([{ "id": 3, "gitlab_repository_ids": [7] }])),
            // …and the repository's default branch.
            Reply::result(json!([{ "id": 7, "default_branch": "development" }])),
        ],
        vec![
            ok("task-5238-widget"),
            ok("branch set up to track origin"),
            ok("https://git.example.com/group/repo/-/merge_requests/404"),
        ],
    );
    assert_eq!(
        backend.ensure_merge_request(&mr_request()).unwrap(),
        Some("https://git.example.com/group/repo/-/merge_requests/404".to_string())
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].args[2], "rev-parse");
    assert_eq!(calls[1].args[2], "push");
    assert_eq!(
        &calls[2].args,
        &[
            "mr",
            "create",
            "--fill",
            "--yes",
            "--source-branch",
            "task-5238-widget",
            "--target-branch",
            "development",
        ]
    );
}

#[test]
fn a_failed_glab_call_is_reported_rather_than_swallowed() {
    let (backend, _server, _runner) = backend_with(
        vec![
            gitlab_fields(""),
            Reply::result(json!([{ "id": 3, "gitlab_repository_ids": [] }])),
        ],
        vec![
            ok("task-5238-widget"),
            failed("! [rejected] task-5238-widget -> non-fast-forward"),
        ],
    );
    let error = backend.ensure_merge_request(&mr_request()).unwrap_err();
    assert!(error.contains("non-fast-forward"), "{error}");
}

#[test]
fn the_target_branch_falls_back_config_then_project_then_development() {
    assert_eq!(
        resolve_target_branch(Some("release"), Some("main".into())),
        "release"
    );
    assert_eq!(resolve_target_branch(None, Some("main".into())), "main");
    assert_eq!(resolve_target_branch(None, None), "development");
    // An empty configured value is not a choice.
    assert_eq!(resolve_target_branch(Some(""), Some("main".into())), "main");
    assert_eq!(
        resolve_target_branch(Some(""), Some("".into())),
        "development"
    );
}

#[test]
fn setting_a_task_state_writes_only_the_state() {
    let (backend, server) = backend(vec![Reply::result(json!(true))]);
    backend
        .set_task_state(5238, crate::odoo::task_state::COMPLETE)
        .expect("written");
    let args = server.args(1);
    assert_eq!(args[4], json!("write"));
    assert_eq!(args[5][1], json!({ "state": "03_approved" }));
}
