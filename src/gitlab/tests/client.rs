//! The client: the retry, the host in the child's environment, the timeouts,
//! and what happens with no GitLab configured.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::{client, failed, json_ok, ok, FakeRunner};
use crate::gitlab::{
    Gitlab, GitlabError, MergeOptions, MrScope, DEFAULT_TIMEOUT, HOST_ENV, MERGE_TIMEOUT,
};
use crate::term::Runner;

fn mr_record(status: &str) -> serde_json::Value {
    json!({
        "iid": 403,
        "title": "Widget rollout",
        "web_url": "https://git.example.com/group/repo/-/merge_requests/403",
        "state": "opened",
        "source_branch": "task-4242-widget",
        "target_branch": "development",
        "has_conflicts": false,
        "detailed_merge_status": status,
        "head_pipeline": { "status": "success" },
        "author": { "username": "dev" },
    })
}

#[test]
fn reading_an_mr_retries_exactly_once_while_gitlab_is_still_checking() {
    // Reading an MR is what kicks the mergeability check, so the first answer
    // is routinely "checking". One retry turns that into a real verdict.
    let (gitlab, runner) = client(vec![
        json_ok(mr_record("checking")),
        json_ok(mr_record("mergeable")),
    ]);
    let mr = gitlab.fetch_mr("group/repo", 403).unwrap();
    assert_eq!(mr.merge_status, "mergeable");
    assert_eq!(runner.call_count(), 2, "exactly one retry");
    assert_eq!(
        runner.calls()[0].args,
        vec!["api", "projects/group%2Frepo/merge_requests/403"]
    );
}

#[test]
fn every_pending_verdict_triggers_the_retry_and_a_settled_one_does_not() {
    for status in ["checking", "unchecked", "preparing"] {
        let (gitlab, runner) = client(vec![
            json_ok(mr_record(status)),
            json_ok(mr_record("mergeable")),
        ]);
        gitlab.fetch_mr("group/repo", 1).unwrap();
        assert_eq!(runner.call_count(), 2, "{status} should be retried");
    }
    for status in ["mergeable", "conflict", "ci_still_running"] {
        let (gitlab, runner) = client(vec![json_ok(mr_record(status))]);
        gitlab.fetch_mr("group/repo", 1).unwrap();
        assert_eq!(runner.call_count(), 1, "{status} is settled");
    }
}

#[test]
fn the_retry_never_runs_twice_and_a_failed_retry_keeps_the_first_answer() {
    // The read itself started the check; a second wait would not make GitLab
    // faster, and throwing away a usable record over a transient failure would
    // leave the row with no verdict at all.
    let (gitlab, runner) = client(vec![json_ok(mr_record("checking")), failed("timed out")]);
    let mr = gitlab.fetch_mr("group/repo", 403).unwrap();
    assert_eq!(mr.merge_status, "checking");
    assert_eq!(runner.call_count(), 2);
}

#[test]
fn the_merge_request_record_collapses_gitlabs_duplicate_field_names() {
    let (gitlab, _runner) = client(vec![json_ok(json!({
        "iid": 7,
        "state": "opened",
        // The pre-14.0 spellings, which some self-managed servers still send.
        "work_in_progress": true,
        "merge_status": "cannot_be_merged",
        "pipeline": { "status": "failed" },
        "has_conflicts": true,
    }))]);
    let mr = gitlab.fetch_mr("group/repo", 7).unwrap();
    assert!(mr.draft, "work_in_progress is draft");
    assert_eq!(mr.merge_status, "cannot_be_merged");
    assert_eq!(mr.pipeline, "failed");
    assert!(mr.conflicts);
}

#[test]
fn the_host_travels_in_the_childs_environment_and_nothing_else_does() {
    let (gitlab, runner) = client(vec![json_ok(mr_record("mergeable"))]);
    gitlab.fetch_mr("group/repo", 1).unwrap();
    let call = runner.calls().remove(0);
    assert_eq!(call.program, "glab");
    assert_eq!(
        call.env,
        vec![(HOST_ENV.to_string(), "git.example.com".to_string())]
    );
    assert_eq!(call.timeout, DEFAULT_TIMEOUT);
    assert_eq!(call.cwd, None);
}

#[test]
fn with_no_host_configured_every_call_is_a_hint_not_a_crash() {
    let runner = FakeRunner::new(vec![ok("")]);
    let gitlab = Gitlab::new(None, Arc::clone(&runner) as Arc<dyn Runner>);
    assert!(!gitlab.is_configured());
    assert_eq!(
        gitlab.fetch_mr("group/repo", 1).unwrap_err(),
        GitlabError::NotConfigured
    );
    assert_eq!(
        gitlab.fetch_open_mrs(MrScope::CreatedByMe).unwrap_err(),
        GitlabError::NotConfigured
    );
    assert_eq!(runner.call_count(), 0, "nothing was run");
    assert!(GitlabError::NotConfigured
        .to_string()
        .contains("gitlabHost"));
}

#[test]
fn merging_uses_the_long_timeout_and_reports_gitlabs_own_first_line() {
    let (gitlab, runner) = client(vec![failed(
        "! Could not merge: pipeline must succeed\nrun `glab ci status` for details",
    )]);
    let error = gitlab
        .merge_mr("group/repo", 403, MergeOptions::default())
        .unwrap_err();
    assert_eq!(
        error,
        GitlabError::Failed("! Could not merge: pipeline must succeed".to_string())
    );
    assert_eq!(runner.calls()[0].timeout, MERGE_TIMEOUT);
}

#[test]
fn a_successful_merge_hands_back_what_glab_printed() {
    let (gitlab, runner) = client(vec![ok("Merged !403")]);
    assert_eq!(
        gitlab
            .merge_mr("group/repo", 403, MergeOptions::default())
            .unwrap(),
        "Merged !403"
    );
    assert_eq!(
        runner.calls()[0].args,
        vec!["mr", "merge", "403", "--repo", "group/repo", "--yes"]
    );
}

#[test]
fn the_open_mr_listing_maps_rows_and_tolerates_a_non_array_body() {
    let (gitlab, _runner) = client(vec![json_ok(json!([
        {
            "iid": 403,
            "title": "Widget rollout",
            "web_url": "https://git.example.com/group/repo/-/merge_requests/403",
            "references": { "full": "group/repo!403" },
            "target_branch": "development",
            "draft": true,
            "updated_at": "2026-09-16T10:00:00Z",
        },
        {
            "iid": 12,
            "title": "Atlas sync",
            "web_url": "https://git.example.com/other/atlas/-/merge_requests/12",
        },
    ]))]);
    let mrs = gitlab.fetch_open_mrs(MrScope::CreatedByMe).unwrap();
    assert_eq!(mrs.len(), 2);
    assert_eq!(mrs[0].project, "group/repo");
    assert!(mrs[0].draft);
    // No `references`, so the project comes back out of the URL.
    assert_eq!(mrs[1].project, "other/atlas");
    assert!(!mrs[1].draft);

    let (gitlab, _runner) = client(vec![json_ok(json!({ "message": "403 Forbidden" }))]);
    assert!(gitlab
        .fetch_open_mrs(MrScope::CreatedByMe)
        .unwrap()
        .is_empty());
}

#[test]
fn output_that_is_not_json_is_its_own_error() {
    let (gitlab, _runner) = client(vec![ok("glab: command not found")]);
    assert_eq!(
        gitlab.fetch_mr("group/repo", 1).unwrap_err(),
        GitlabError::Unparseable
    );
}

#[test]
fn opening_a_merge_request_pushes_first_and_reports_the_url() {
    let (gitlab, runner) = client(vec![
        ok("branch 'task-4242-widget' set up to track 'origin/task-4242-widget'."),
        ok("!404 created\nhttps://git.example.com/group/repo/-/merge_requests/404\n"),
    ]);
    let url = gitlab
        .create_mr(Path::new("/repo"), "task-4242-widget", "development")
        .unwrap();
    assert_eq!(
        url.as_deref(),
        Some("https://git.example.com/group/repo/-/merge_requests/404")
    );

    let calls = runner.calls();
    assert_eq!(calls[0].program, "git");
    assert_eq!(
        &calls[0].args[2..],
        ["push", "-u", "origin", "task-4242-widget"]
    );
    assert_eq!(calls[1].program, "glab");
    // `mr create` reads the repository's remote, so it runs in the repository.
    assert_eq!(calls[1].cwd.as_deref(), Some("/repo"));
}

#[test]
fn a_failed_push_stops_the_merge_request_from_being_attempted() {
    let (gitlab, runner) = client(vec![failed(
        "! [rejected] task-4242-widget -> non-fast-forward",
    )]);
    assert!(gitlab
        .create_mr(Path::new("/repo"), "task-4242-widget", "development")
        .is_err());
    assert_eq!(runner.call_count(), 1, "glab must not have run");
}

#[test]
fn the_current_branch_is_trimmed_and_a_detached_head_is_no_branch() {
    let (gitlab, _runner) = client(vec![ok("task-4242-widget\n")]);
    assert_eq!(
        gitlab.current_branch(Path::new("/repo")).as_deref(),
        Some("task-4242-widget")
    );

    let (gitlab, _runner) = client(vec![ok("HEAD")]);
    assert_eq!(gitlab.current_branch(Path::new("/repo")), None);

    let (gitlab, _runner) = client(vec![failed("not a git repository")]);
    assert_eq!(gitlab.current_branch(Path::new("/tmp")), None);
}

#[test]
fn the_recheck_delay_is_a_real_pause_by_default() {
    // The zero delay is a test affordance; the shipped value has to be long
    // enough for GitLab to have finished, and it is the Node wrapper's.
    assert_eq!(crate::gitlab::RECHECK_DELAY, Duration::from_millis(1200));
}
