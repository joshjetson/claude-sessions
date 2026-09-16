//! `fetch_board` and the queries around it: grouping, subtask nesting, the
//! filters, the metadata caches, and the record decoding underneath.

use serde_json::{json, Value};

use super::stub::Reply;
use super::{client_with, record, stage_rows};
use crate::odoo::FetchBoardOptions;

fn board_replies(tasks: Value) -> Vec<Reply> {
    vec![
        Reply::result(tasks),
        Reply::result(stage_rows()),
        Reply::result(json!([{ "id": 11, "name": "auto_review" }])),
    ]
}

#[test]
fn tasks_are_grouped_by_project_and_stage_with_the_stage_sequence() {
    let (client, server) = client_with(board_replies(json!([
        record(5944, "Fix the export", (3, "Beacon"), (2, "In Progress")),
        record(
            5217,
            "Template picker",
            (3, "Beacon"),
            (3, "Quality Assurance")
        ),
        record(4033, "Audit trail", (9, "Ledger"), (2, "In Progress")),
    ])));

    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();

    assert_eq!(board.task_count, 3);
    assert!(!board.truncated);
    assert_eq!(
        board.projects.keys().collect::<Vec<_>>(),
        vec!["Beacon", "Ledger"]
    );
    let beacon = &board.projects["Beacon"];
    assert_eq!(beacon.project_id, 3);
    assert_eq!(beacon.stages["In Progress"].stage_id, 2);
    assert_eq!(beacon.stages["In Progress"].sequence, 2);
    assert_eq!(beacon.stages["Quality Assurance"].sequence, 3);
    assert_eq!(beacon.stages["In Progress"].tasks[0].id, 5944);

    // My board is filtered by assignment, so it is never limited or truncated.
    let kwargs = &server.requests()[1]["params"]["args"][6];
    assert_eq!(kwargs["limit"], json!(0));
    assert_eq!(kwargs["order"], json!("write_date desc"));
    assert_eq!(server.args(1)[5][0], json!([["user_ids", "=", 7]]));
}

#[test]
fn a_stage_with_no_sequence_sorts_last() {
    let (client, _server) = client_with(board_replies(json!([record(
        1,
        "Odd one",
        (3, "Beacon"),
        (77, "Somewhere New")
    )])));
    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    assert_eq!(
        board.projects["Beacon"].stages["Somewhere New"].sequence,
        999
    );
}

#[test]
fn subtasks_nest_under_their_parent_and_leave_the_top_level() {
    let mut parent = record(5944, "Parent", (3, "Beacon"), (2, "In Progress"));
    parent["child_ids"] = json!([8801, 8802]);
    let child_on_my_board = record(8802, "Mine too", (3, "Beacon"), (2, "In Progress"));

    let (client, server) = client_with(vec![
        Reply::result(json!([parent, child_on_my_board])),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
        // The children query: 8801 belongs to someone else, so only this call
        // sees it.
        Reply::result(json!([
            record(8801, "Someone else's", (3, "Beacon"), (2, "In Progress")),
            record(8802, "Mine too", (3, "Beacon"), (2, "In Progress")),
        ])),
    ]);

    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    let stage = &board.projects["Beacon"].stages["In Progress"];
    assert_eq!(
        stage.tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![5944],
        "the nested child stayed at the top level too"
    );
    assert_eq!(
        stage.tasks[0]
            .subtasks
            .iter()
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        vec![8801, 8802],
        "subtasks are ordered by id"
    );
    assert_eq!(server.calls().last().unwrap().0, "project.task");
}

#[test]
fn a_failed_children_query_costs_nesting_not_the_board() {
    let mut parent = record(5944, "Parent", (3, "Beacon"), (2, "In Progress"));
    parent["child_ids"] = json!([8801]);
    let (client, _server) = client_with(vec![
        Reply::result(json!([parent])),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
        Reply::error("no access to that task"),
    ]);

    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    let stage = &board.projects["Beacon"].stages["In Progress"];
    assert_eq!(stage.tasks.len(), 1);
    assert!(stage.tasks[0].subtasks.is_empty());
}

#[test]
fn hidden_stages_are_dropped_client_side_and_hidden_states_in_the_domain() {
    let (client, server) = client_with(board_replies(json!([
        record(1, "Shipped", (3, "Beacon"), (9, "Deployed")),
        record(2, "Working", (3, "Beacon"), (2, "In Progress")),
    ])));

    let board = client
        .fetch_board(&FetchBoardOptions {
            hide_stages: vec!["deployed".to_string()],
            hide_states: vec!["1_done".to_string(), "1_canceled".to_string()],
            ..FetchBoardOptions::default()
        })
        .unwrap();

    assert_eq!(
        board.projects["Beacon"].stages.keys().collect::<Vec<_>>(),
        vec!["In Progress"],
        "the hidden stage was kept"
    );
    // The raw count is what Odoo returned, before the client-side filter.
    assert_eq!(board.task_count, 2);
    assert_eq!(
        server.args(1)[5][0][1],
        json!(["state", "not in", ["1_done", "1_canceled"]])
    );
}

#[test]
fn the_all_view_resolves_project_names_and_reports_truncation() {
    let (client, server) = client_with(vec![
        // getProjects, for the include filter.
        Reply::result(json!([
            { "id": 3, "name": "Beacon" },
            { "id": 9, "name": "Ledger" },
        ])),
        Reply::result(json!([
            record(1, "One", (3, "Beacon"), (2, "In Progress")),
            record(2, "Two", (3, "Beacon"), (2, "In Progress")),
        ])),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
    ]);

    let board = client
        .fetch_board(&FetchBoardOptions {
            mine_only: false,
            include: vec!["Beacon".to_string()],
            limit: 2,
            ..FetchBoardOptions::default()
        })
        .unwrap();

    assert_eq!(server.args(2)[5][0], json!([["project_id", "in", [3]]]));
    // Scoped by an allowlist, so the limit is lifted…
    assert_eq!(server.requests()[2]["params"]["args"][6]["limit"], json!(0));
    // …and truncation only ever describes the unscoped view.
    assert!(!board.truncated);
}

#[test]
fn an_unscoped_all_view_keeps_its_limit_and_reports_truncation() {
    let (client, server) = client_with(board_replies(json!([
        record(1, "One", (3, "Beacon"), (2, "In Progress")),
        record(2, "Two", (3, "Beacon"), (2, "In Progress")),
    ])));

    let board = client
        .fetch_board(&FetchBoardOptions {
            mine_only: false,
            limit: 2,
            ..FetchBoardOptions::default()
        })
        .unwrap();

    assert_eq!(server.requests()[1]["params"]["args"][6]["limit"], json!(2));
    assert!(board.truncated, "a full page must report as truncated");
}

#[test]
fn an_include_list_that_matches_nothing_returns_nothing() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "id": 3, "name": "Beacon" }])),
        Reply::result(json!([])),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
    ]);

    client
        .fetch_board(&FetchBoardOptions {
            mine_only: false,
            include: vec!["Typo".to_string()],
            ..FetchBoardOptions::default()
        })
        .unwrap();

    // Id 0 exists nowhere, so an unmatched allowlist cannot fall open.
    assert_eq!(server.args(2)[5][0], json!([["project_id", "in", [0]]]));
}

#[test]
fn ignored_projects_are_excluded_when_they_resolve() {
    let (client, server) = client_with(vec![
        Reply::result(json!([{ "id": 9, "name": "Ledger" }])),
        Reply::result(json!([])),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
    ]);

    client
        .fetch_board(&FetchBoardOptions {
            ignore: vec!["Ledger".to_string()],
            ..FetchBoardOptions::default()
        })
        .unwrap();

    assert_eq!(
        server.args(2)[5][0][1],
        json!(["project_id", "not in", [9]])
    );
}

#[test]
fn story_points_prefer_the_integer_and_fall_back_to_the_selection() {
    let mut integer = record(1, "Integer", (3, "Beacon"), (2, "In Progress"));
    integer["x_studio_story_points_1"] = json!(5);
    integer["x_studio_story_points"] = json!("8");

    let mut selection = record(2, "Selection", (3, "Beacon"), (2, "In Progress"));
    selection["x_studio_story_points_1"] = json!(false);
    selection["x_studio_story_points"] = json!("3 points");

    let mut zero = record(3, "Unestimated", (3, "Beacon"), (2, "In Progress"));
    zero["x_studio_story_points_1"] = json!(0);
    zero["x_studio_story_points"] = json!("none");

    let (client, _server) = client_with(board_replies(json!([integer, selection, zero])));
    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    let tasks = &board.projects["Beacon"].stages["In Progress"].tasks;

    assert_eq!(tasks[0].story_points, Some(5), "the integer field wins");
    assert_eq!(tasks[1].story_points, Some(3), "the selection is parsed");
    assert_eq!(tasks[2].story_points, None, "zero is unestimated, not 0sp");
}

#[test]
fn tags_deadlines_and_dependency_counts_decode() {
    let mut task = record(5238, "Blocked one", (3, "Beacon"), (1, "Approved to Start"));
    task["tag_ids"] = json!([11]);
    task["date_deadline"] = json!("2026-09-20");
    task["depend_on_ids"] = json!([4034, 4035]);
    task["depend_on_count"] = json!(2);
    task["closed_depend_on_count"] = json!(1);
    task["priority"] = json!("1");

    let (client, _server) = client_with(board_replies(json!([task])));
    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    let task = &board.projects["Beacon"].stages["Approved to Start"].tasks[0];

    assert_eq!(task.tags, vec!["auto_review".to_string()]);
    assert_eq!(task.deadline.as_deref(), Some("2026-09-20"));
    assert_eq!(task.blocked_by, vec![4034, 4035]);
    assert_eq!(task.blocker_count, 2);
    assert_eq!(task.open_blocker_count, 1);
    assert_eq!(task.priority.as_deref(), Some("1"));
}

#[test]
fn a_task_with_no_stage_reads_as_no_stage() {
    let mut task = record(1, "Loose", (3, "Beacon"), (0, ""));
    task["stage_id"] = json!(false);
    let (client, _server) = client_with(board_replies(json!([task])));
    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    assert!(board.projects["Beacon"].stages.contains_key("No stage"));
}

#[test]
fn a_task_with_no_project_never_reaches_the_board() {
    let mut task = record(1, "Private", (0, ""), (2, "In Progress"));
    task["project_id"] = json!(false);
    let (client, _server) = client_with(board_replies(json!([task])));
    let board = client.fetch_board(&FetchBoardOptions::default()).unwrap();
    assert!(board.projects.is_empty());
    assert_eq!(board.task_count, 1);
}

#[test]
fn stage_and_tag_metadata_are_cached_until_invalidated() {
    let tasks = json!([record(1, "One", (3, "Beacon"), (2, "In Progress"))]);
    let (client, server) = client_with(vec![
        Reply::result(tasks.clone()),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
        Reply::result(tasks.clone()),
        Reply::result(tasks.clone()),
        Reply::result(stage_rows()),
        Reply::result(json!([])),
    ]);

    client.fetch_board(&FetchBoardOptions::default()).unwrap();
    client.fetch_board(&FetchBoardOptions::default()).unwrap();
    assert_eq!(
        server.calls(),
        vec![
            ("project.task".to_string(), "search_read".to_string()),
            ("project.task.type".to_string(), "search_read".to_string()),
            ("project.tags".to_string(), "search_read".to_string()),
            ("project.task".to_string(), "search_read".to_string()),
        ],
        "the metadata was re-fetched on the second refresh"
    );

    // Node had no way to do this, so a renamed stage stayed wrong until restart.
    client.invalidate_metadata();
    client.fetch_board(&FetchBoardOptions::default()).unwrap();
    assert_eq!(
        server.calls().len(),
        7,
        "invalidating the caches must refetch the metadata"
    );
}
