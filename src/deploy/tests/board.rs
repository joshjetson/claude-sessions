//! Loading the board: the Odoo query, the project-path recovery, and the live
//! overlay. The Odoo client is real and points at a loopback stub; the GitLab
//! client is real and points at a recorded runner.

use serde_json::json;

use crate::deploy::{
    enrich_with_live_mrs, fetch_deploy_board, to_deploy_task, DeployProjectSpec, DEPLOY_STAGE,
    DEPLOY_TASK_FIELDS,
};
use crate::gitlab::tests::{client as gitlab_client, failed, json_ok};
use crate::odoo::tests::{client_with, stub::Reply};

fn specs(names: &[&str]) -> Vec<DeployProjectSpec> {
    names
        .iter()
        .map(|name| DeployProjectSpec {
            name: (*name).to_string(),
            command: "./scripts/deploy-prod.sh".to_string(),
            target_branch: "main".to_string(),
        })
        .collect()
}

fn task_row(id: i64, project: (i64, &str), mr: serde_json::Value) -> serde_json::Value {
    let mut row = json!({
        "id": id,
        "name": format!("Task {id}"),
        "state": "01_in_progress",
        "project_id": [project.0, project.1],
        "stage_id": [9, DEPLOY_STAGE],
        "priority": "0",
        "date_deadline": false,
        "gitlab_branch_name": format!("task-{id}"),
        "gitlab_merge_request_url": false,
        "gitlab_merge_request_state": false,
        "gitlab_merge_request_iid": false,
    });
    for (key, value) in mr.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

#[test]
fn no_configured_projects_is_an_unconfigured_board_and_no_query_at_all() {
    let (client, server) = client_with(vec![]);
    let board = fetch_deploy_board(&client, &[]).unwrap();
    assert!(!board.configured);
    assert!(board.projects.is_empty());
    assert_eq!(server.request_count(), 0, "Odoo was not asked anything");
}

#[test]
fn a_configured_project_with_nothing_outstanding_still_appears() {
    // That IS the "clear to deploy" signal — hiding it hides the answer.
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([])),
    ]);
    let board = fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    assert!(board.configured);
    assert_eq!(board.project_names, ["Aurora"]);
    let project = &board.projects["Aurora"];
    assert_eq!(project.project_id, 3);
    assert!(!project.missing);
    assert!(project.tasks.is_empty());
    assert_eq!(project.command, "./scripts/deploy-prod.sh");
    assert_eq!(project.target_branch, "main");
}

#[test]
fn a_project_the_config_names_but_odoo_does_not_have_is_marked_missing() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([])),
    ]);
    let board = fetch_deploy_board(&client, &specs(&["Aurora", "Nowhere"])).unwrap();
    assert!(!board.projects["Aurora"].missing);
    assert!(board.projects["Nowhere"].missing);
    assert_eq!(board.projects["Nowhere"].project_id, 0);
    // Only the project that exists is queried for.
    let domain = &server.args(2)[5][0];
    assert_eq!(domain[0], json!(["project_id", "in", [3]]));
}

#[test]
fn the_query_asks_for_deployed_and_unfinished_only() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([])),
    ]);
    fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    assert_eq!(
        server.call_target(2),
        ("project.task".into(), "search_read".into())
    );
    let args = server.args(2);
    let domain = &args[5][0];
    assert_eq!(domain[1], json!(["stage_id.name", "=", "Deployed"]));
    assert_eq!(
        domain[2],
        json!(["state", "not in", ["03_approved", "1_done", "1_canceled"]])
    );
    assert_eq!(args[6]["order"], json!("write_date desc"));
    assert_eq!(args[6]["fields"], json!(DEPLOY_TASK_FIELDS));
}

#[test]
fn project_names_match_case_insensitively_and_keep_the_configured_spelling() {
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Orbit Media" }])),
        Reply::result(json!([task_row(1, (3, "Orbit Media"), json!({}))])),
    ]);
    let board = fetch_deploy_board(&client, &specs(&["orbit media"])).unwrap();
    assert_eq!(board.project_names, ["orbit media"]);
    assert_eq!(board.projects["orbit media"].tasks.len(), 1);
}

#[test]
fn a_task_row_decodes_its_merge_request_from_the_url() {
    let task = to_deploy_task(&task_row(
        4242,
        (3, "Aurora"),
        json!({
            "gitlab_merge_request_url": "https://git.example.com/group/repo/-/merge_requests/403",
            "gitlab_merge_request_state": "opened",
        }),
    ));
    assert_eq!(task.mr_iid, Some(403));
    assert_eq!(task.mr_project_path, "group/repo");
    assert_eq!(task.mr_state, "opened");
    assert_eq!(task.state_label, "In Progress");
    assert_eq!(task.stage_name, "Deployed");
    assert_eq!(task.branch, "task-4242");
    assert!(task.mr.is_none(), "not loaded until GitLab is read");
}

#[test]
fn a_row_with_only_an_iid_keeps_it_and_leaves_the_path_empty() {
    let task = to_deploy_task(&task_row(
        4242,
        (3, "Aurora"),
        json!({ "gitlab_merge_request_iid": 403 }),
    ));
    assert_eq!(task.mr_iid, Some(403));
    assert_eq!(task.mr_project_path, "");
    assert_eq!(task.mr_url, "");
}

#[test]
fn a_missing_project_path_is_recovered_only_when_the_project_has_one_repo() {
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(
            4242,
            (3, "Aurora"),
            json!({ "gitlab_merge_request_iid": 403 })
        )])),
        // project.project read -> one linked repository
        Reply::result(json!([{ "id": 3, "gitlab_repository_ids": [7] }])),
        Reply::result(json!([{ "id": 7, "full_path": "group/repo" }])),
    ]);
    let board = fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    assert_eq!(
        board.projects["Aurora"].tasks[0].mr_project_path,
        "group/repo"
    );
}

#[test]
fn two_linked_repositories_leave_the_path_unresolved_rather_than_guessed() {
    // With several repos there is no way to tell which one the iid belongs to,
    // and merging into the wrong repository is unrecoverable.
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(
            4242,
            (3, "Aurora"),
            json!({ "gitlab_merge_request_iid": 403 })
        )])),
        Reply::result(json!([{ "id": 3, "gitlab_repository_ids": [7, 8] }])),
        Reply::result(json!([
            { "id": 7, "full_path": "group/repo" },
            { "id": 8, "full_path": "group/other" },
        ])),
    ]);
    let board = fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    assert_eq!(board.projects["Aurora"].tasks[0].mr_project_path, "");
    assert_eq!(
        crate::deploy::merge_readiness(&board.projects["Aurora"].tasks[0]).reason,
        "MR linked but its GitLab project is unknown"
    );
}

#[test]
fn the_repo_lookup_is_skipped_when_no_task_needs_it() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(
            4242,
            (3, "Aurora"),
            json!({
                "gitlab_merge_request_url":
                    "https://git.example.com/group/repo/-/merge_requests/403"
            })
        )])),
    ]);
    fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    assert_eq!(
        server.calls(),
        vec![
            ("project.project".to_string(), "search_read".to_string()),
            ("project.task".to_string(), "search_read".to_string()),
        ],
        "no project.project read for repositories"
    );
}

// --- the live overlay -------------------------------------------------------

fn board_with_one_mr() -> crate::types::DeployBoard {
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(
            4242,
            (3, "Aurora"),
            json!({
                "gitlab_merge_request_url":
                    "https://git.example.com/group/repo/-/merge_requests/403",
                "gitlab_merge_request_state": "opened",
            })
        )])),
    ]);
    fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap()
}

#[test]
fn the_live_merge_request_state_beats_odoos_cached_one() {
    // Odoo's field only updates when the integration syncs, so it is routinely
    // a merge behind. That staleness is the reason this overlay exists.
    let mut board = board_with_one_mr();
    assert_eq!(board.projects["Aurora"].tasks[0].mr_state, "opened");

    let (gitlab, _runner) = gitlab_client(vec![json_ok(json!({
        "iid": 403,
        "state": "merged",
        "web_url": "https://git.example.com/group/repo/-/merge_requests/403",
        "detailed_merge_status": "mergeable",
    }))]);
    enrich_with_live_mrs(&gitlab, &mut board);

    let task = &board.projects["Aurora"].tasks[0];
    assert_eq!(task.mr_state, "merged", "live wins");
    assert_eq!(task.mr.as_ref().unwrap().state, "merged");
    assert!(task.mr_error.is_none());
}

#[test]
fn a_failed_lookup_keeps_the_stale_value_and_records_why() {
    let mut board = board_with_one_mr();
    let (gitlab, _runner) = gitlab_client(vec![failed("404 Not Found")]);
    enrich_with_live_mrs(&gitlab, &mut board);

    let task = &board.projects["Aurora"].tasks[0];
    assert_eq!(task.mr_state, "opened", "Odoo's value stayed");
    assert!(task.mr.is_none());
    assert_eq!(task.mr_error.as_deref(), Some("404 Not Found"));
    assert_eq!(
        crate::deploy::merge_readiness(task).reason,
        "MR status unavailable (404 Not Found)"
    );
}

#[test]
fn a_url_odoo_never_stored_is_recovered_from_the_live_record() {
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(
            4242,
            (3, "Aurora"),
            json!({ "gitlab_merge_request_iid": 403 })
        )])),
        Reply::result(json!([{ "id": 3, "gitlab_repository_ids": [7] }])),
        Reply::result(json!([{ "id": 7, "full_path": "group/repo" }])),
    ]);
    let mut board = fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    let (gitlab, _runner) = gitlab_client(vec![json_ok(json!({
        "iid": 403,
        "state": "opened",
        "web_url": "https://git.example.com/group/repo/-/merge_requests/403",
        "detailed_merge_status": "mergeable",
    }))]);
    enrich_with_live_mrs(&gitlab, &mut board);
    assert_eq!(
        board.projects["Aurora"].tasks[0].mr_url,
        "https://git.example.com/group/repo/-/merge_requests/403"
    );
}

#[test]
fn tasks_with_no_merge_request_are_never_looked_up() {
    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Aurora" }])),
        Reply::result(json!([task_row(4242, (3, "Aurora"), json!({}))])),
    ]);
    let mut board = fetch_deploy_board(&client, &specs(&["Aurora"])).unwrap();
    let (gitlab, runner) = gitlab_client(vec![json_ok(json!({ "iid": 1 }))]);
    enrich_with_live_mrs(&gitlab, &mut board);
    assert_eq!(runner.call_count(), 0);
}
