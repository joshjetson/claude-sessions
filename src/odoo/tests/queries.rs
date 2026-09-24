//! The queries either side of `fetch_board`: blockers, stages, task detail, the
//! GitLab links, the writes, and the project list.

use serde_json::json;

use super::stub::Reply;
use super::{client_with, record};
use crate::odoo::TaskGitlab;

#[test]
fn stages_for_tasks_dedupes_ids_and_names_the_missing_stage() {
    let (client, server) = client_with(vec![Reply::result(json!([
        { "id": 5944, "stage_id": [3, "Quality Assurance"] },
        { "id": 4033, "stage_id": false },
    ]))]);

    let stages = client
        .fetch_stages_for_tasks(&[5944, 4033, 5944, 0])
        .unwrap();
    assert_eq!(stages[&5944], "Quality Assurance");
    assert_eq!(stages[&4033], "No stage");

    let mut ids: Vec<i64> = server.args(1)[5][0]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![4033, 5944], "a duplicate or a zero id was sent");
}

#[test]
fn blockers_report_whether_they_have_landed() {
    let (client, _server) = client_with(vec![Reply::result(json!([
        { "id": 4034, "name": "Data model", "stage_id": [9, "Deployed"], "state": "03_approved" },
        { "id": 4035, "name": "Migration", "stage_id": [2, "In Progress"], "state": "01_in_progress" },
        { "id": 4036, "name": "Spike", "stage_id": false, "state": "1_canceled" },
    ]))]);

    let blockers = client.fetch_blockers(&[4034, 4035, 4036]).unwrap();
    assert_eq!(blockers[0].stage_name, "Deployed");
    assert!(blockers[0].done, "03_approved counts as closed");
    assert!(!blockers[1].done);
    assert_eq!(blockers[2].stage_name, "No stage");
    assert!(blockers[2].done, "a cancelled blocker no longer blocks");
}

#[test]
fn no_blockers_asks_odoo_nothing() {
    let (client, server) = client_with(vec![]);
    assert!(client.fetch_blockers(&[]).unwrap().is_empty());
    assert!(client.fetch_assigned_in_stages(&[]).unwrap().is_empty());
    assert_eq!(server.request_count(), 0);
}

#[test]
fn assigned_in_stages_asks_by_stage_name_and_excludes_finished_tasks() {
    let (client, server) = client_with(vec![
        Reply::result(json!([record(
            5944,
            "Pick me up",
            (3, "Aurora"),
            (1, "Approved to Start")
        )])),
        Reply::result(json!([])),
    ]);

    let tasks = client
        .fetch_assigned_in_stages(&["Approved to Start".to_string()])
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        server.args(1)[5][0],
        json!([
            ["user_ids", "=", 7],
            ["stage_id.name", "in", ["Approved to Start"]],
            ["state", "not in", ["1_done", "1_canceled"]],
        ])
    );
    assert_eq!(
        server.requests()[1]["params"]["args"][6]["limit"],
        json!(200)
    );
}

#[test]
fn task_detail_reads_the_fields_the_pane_shows() {
    let (client, server) = client_with(vec![Reply::result(json!([{
        "id": 5944,
        "name": "Fix the export",
        "description": "<p>Broken since Tuesday.</p>",
        "stage_id": [2, "In Progress"],
        "project_id": [3, "Aurora"],
        "priority": "1",
        "date_deadline": "2026-09-20",
    }]))]);

    let detail = client.get_task_detail(5944).unwrap().unwrap();
    assert_eq!(detail.name, "Fix the export");
    assert_eq!(detail.description, "<p>Broken since Tuesday.</p>");
    assert_eq!(detail.stage_name, "In Progress");
    assert_eq!(detail.project_name, "Aurora");
    assert_eq!(detail.deadline.as_deref(), Some("2026-09-20"));
    assert_eq!(
        server.call_target(1),
        ("project.task".into(), "read".into())
    );
}

#[test]
fn a_missing_task_reads_as_no_detail() {
    let (client, _server) = client_with(vec![Reply::result(json!([]))]);
    assert_eq!(client.get_task_detail(1).unwrap(), None);
}

#[test]
fn gitlab_fields_come_back_when_the_integration_is_installed() {
    let (client, _server) = client_with(vec![Reply::result(json!([{
        "gitlab_branch_name": "task-5944-fix-the-export",
        "gitlab_merge_request_url": "https://git.example/group/repo/-/merge_requests/403",
        "gitlab_merge_request_state": "opened",
    }]))]);

    assert_eq!(
        client.get_task_gitlab(5944),
        Some(TaskGitlab {
            branch_name: "task-5944-fix-the-export".to_string(),
            merge_request_url: "https://git.example/group/repo/-/merge_requests/403".to_string(),
            merge_request_state: "opened".to_string(),
        })
    );
}

#[test]
fn a_project_without_the_gitlab_module_reads_as_no_gitlab_info() {
    // Reading a field Odoo does not have raises; the dialog must still open.
    let (client, _server) = client_with(vec![Reply::error("Invalid field 'gitlab_branch_name'")]);
    assert_eq!(client.get_task_gitlab(5944), None);
}

#[test]
fn repo_paths_and_the_default_branch_walk_project_then_repository() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "gitlab_repository_ids": [92, 7] }])),
        Reply::result(json!([{ "full_path": "group/repo" }, { "full_path": "group/other" }])),
    ]);
    assert_eq!(
        client.get_project_repo_paths(3),
        vec!["group/repo".to_string(), "group/other".to_string()]
    );
    assert_eq!(
        server.calls(),
        vec![
            ("project.project".to_string(), "read".to_string()),
            ("gitlab.repository".to_string(), "read".to_string()),
        ]
    );

    let (client, _server) = client_with(vec![
        Reply::result(json!([{ "gitlab_repository_ids": [92] }])),
        Reply::result(json!([{ "default_branch": "development" }])),
    ]);
    assert_eq!(
        client.get_project_default_branch(3),
        Some("development".to_string())
    );
}

#[test]
fn a_project_with_no_linked_repository_answers_empty() {
    let (client, server) = client_with(vec![Reply::result(
        json!([{ "gitlab_repository_ids": [] }]),
    )]);
    assert!(client.get_project_repo_paths(3).is_empty());
    assert_eq!(client.get_project_default_branch(3), None);
    // Two project reads, and no repository read either time.
    assert_eq!(server.calls().len(), 2);
}

#[test]
fn writes_send_the_shapes_odoo_expects() {
    let (client, server) = client_with(vec![Reply::result(json!(true))]);
    client.move_stage(5944, 3).unwrap();
    client
        .set_task_state(5944, crate::odoo::task_state::COMPLETE)
        .unwrap();
    client.post_comment(5944, "<p><b>Done</b></p>").unwrap();

    assert_eq!(server.args(1)[6], json!({}));
    assert_eq!(server.args(1)[5], json!([[5944], { "stage_id": 3 }]));
    assert_eq!(
        server.args(2)[5],
        json!([[5944], { "state": "03_approved" }])
    );
    assert_eq!(
        server.call_target(3),
        ("project.task".into(), "message_post".into())
    );
    assert_eq!(
        server.args(3)[6],
        json!({ "body": "<p><b>Done</b></p>", "body_is_html": true, "message_type": "comment" }),
        "without body_is_html Odoo renders the markup as text"
    );
}

#[test]
fn projects_are_fetched_once_and_ordered_by_name() {
    let (client, server) = client_with(vec![Reply::result(json!([
        { "id": 3, "name": "Aurora" },
        { "id": 9, "name": "Ledger" },
    ]))]);

    let first = client.get_projects().unwrap();
    let second = client.get_projects().unwrap();
    assert_eq!(first, second);
    assert_eq!(first[0].name, "Aurora");
    assert_eq!(server.calls().len(), 1);
    assert_eq!(
        server.requests()[1]["params"]["args"][6]["order"],
        json!("name")
    );
}

#[test]
fn qa_stage_tasks_carry_who_has_them_and_when_they_arrived() {
    let mut mine = record(8101, "Check the export", (3, "Aurora"), (4, "QA"));
    mine["user_ids"] = json!([7, 24]);
    mine["date_last_stage_update"] = json!("2026-09-24 09:15:00");
    let mut theirs = record(8102, "Check the import", (3, "Aurora"), (4, "QA"));
    theirs["user_ids"] = json!([24]);
    theirs["date_last_stage_update"] = json!(false);
    let (client, server) = client_with(vec![
        Reply::result(json!([mine, theirs])),
        Reply::result(json!([])),
    ]);

    let tasks = client
        .fetch_in_qa_stages(&["QA".to_string(), "Acceptance Testing".to_string()])
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks[0].assigned_to_me, "uid 7 is the authenticated user");
    assert_eq!(tasks[0].user_ids, [7, 24]);
    assert_eq!(tasks[0].stage_entered, "2026-09-24 09:15:00");
    assert!(!tasks[1].assigned_to_me);
    assert_eq!(tasks[1].stage_entered, "");

    // Not scoped to the current user, and Complete is excluded with the
    // finished states.
    assert_eq!(
        server.args(1)[5][0],
        json!([
            ["stage_id.name", "in", ["QA", "Acceptance Testing"]],
            ["state", "not in", ["1_done", "1_canceled", "03_approved"]],
        ])
    );
    let options = &server.requests()[1]["params"]["args"][6];
    assert_eq!(options["order"], json!("date_last_stage_update desc"));
    let fields = options["fields"].as_array().unwrap();
    assert!(fields.contains(&json!("user_ids")));
    assert!(fields.contains(&json!("date_last_stage_update")));
}

#[test]
fn no_qa_stages_asks_odoo_nothing() {
    let (client, server) = client_with(vec![]);
    assert!(client.fetch_in_qa_stages(&[]).unwrap().is_empty());
    assert_eq!(server.request_count(), 0);
}

/// `state` is how a developer marks work Complete. The board reads it for the
/// Complete badge, so the board query asks for it.
#[test]
fn a_task_record_carries_its_odoo_state() {
    let mut complete = record(8201, "Done by dev", (3, "Aurora"), (4, "QA"));
    complete["state"] = json!("03_approved");
    let task = crate::odoo::records::to_task(&complete, &|_| None);
    assert_eq!(task.state.as_deref(), Some("03_approved"));

    let missing = record(8202, "Older server", (3, "Aurora"), (4, "QA"));
    assert_eq!(
        crate::odoo::records::to_task(&missing, &|_| None).state,
        None
    );
    assert!(crate::odoo::TASK_FIELDS.contains(&"state"));
}
