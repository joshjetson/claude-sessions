//! The Odoo implementation of [`TaskBackend`], against a loopback JSON-RPC
//! stub. No network, no Odoo — the client is the real one and the server is a
//! `TcpListener` on 127.0.0.1:0.
//!
//! What is pinned here is the one rule the whole stage machinery exists for:
//! **never guess forward**. A stage the project does not have leaves the task
//! exactly where it is and says which name was looked for, because the column
//! after "In Progress" is often "Revision Required" and landing a finished task
//! there is worse than leaving it alone.
//!
//! The merge-request safety net is next door in [`merge_request`], because it
//! is the half that shells out rather than the half that talks to Odoo.

mod merge_request;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::{
    MergeRequestRequest, OdooTaskBackend, StageMove, StageMoveRequest, TaskBackend,
};
use crate::gitlab::tests::{client as gitlab_client, ok, FakeRunner};
use crate::odoo::tests::stub::{Reply, StubServer};
use crate::odoo::{HttpTransport, OdooClient, StageKind};
use crate::term::CommandOutput;
use crate::types::OdooCreds;

/// A backend whose client talks to a stub and whose `glab` never runs. The
/// first reply is the authentication every call begins with.
pub(crate) fn backend(replies: Vec<Reply>) -> (OdooTaskBackend, StubServer) {
    let (backend, server, _runner) = backend_with(replies, Vec::new());
    (backend, server)
}

/// The same, plus a scripted `git`/`glab` so the merge-request path can be
/// driven end to end with no process anywhere near the test.
pub(crate) fn backend_with(
    replies: Vec<Reply>,
    commands: Vec<CommandOutput>,
) -> (OdooTaskBackend, StubServer, Arc<FakeRunner>) {
    let mut all = vec![Reply::result(json!(7))];
    all.extend(replies);
    let server = StubServer::start(all);
    let client = OdooClient::with_transport(
        OdooCreds {
            url: server.url(),
            db: "testdb".into(),
            user: "tester".into(),
            password: "secret".into(),
        },
        Box::new(HttpTransport::new(Duration::from_secs(5))),
    );
    let (gitlab, runner) = gitlab_client(if commands.is_empty() {
        vec![ok("")]
    } else {
        commands
    });
    let dir = tempfile::tempdir().expect("temp dir");
    let config = ConfigHandle::load_from(
        &dir.path().join("config.json"),
        dir.path(),
        EnvOverrides::default(),
    );
    (
        OdooTaskBackend::new(Arc::new(client), gitlab, config),
        server,
        runner,
    )
}

pub(crate) fn mr_request() -> MergeRequestRequest {
    MergeRequestRequest {
        task_id: 5238,
        cwd: "/repo".into(),
        project_id: Some(3),
        project_name: "Repo".into(),
    }
}

/// What Odoo answers a `get_task_gitlab` read with.
pub(crate) fn gitlab_fields(url: &str) -> Reply {
    Reply::result(json!([{
        "id": 5238,
        "gitlab_branch_name": "task-5238-widget",
        "gitlab_merge_request_url": if url.is_empty() { json!(false) } else { json!(url) },
        "gitlab_merge_request_state": false,
    }]))
}

fn stages() -> Reply {
    Reply::result(json!([
        { "id": 1, "name": "Approved to Start", "sequence": 1 },
        { "id": 2, "name": "In Progress", "sequence": 2 },
        { "id": 3, "name": "Quality Assurance", "sequence": 3 },
    ]))
}

fn request(kind: StageKind, stage_id: Option<i64>) -> StageMoveRequest {
    StageMoveRequest {
        project_id: Some(3),
        stage_id,
        project_name: "Repo".into(),
        ..match kind {
            StageKind::Done => StageMoveRequest::to_done(5238),
            StageKind::InProgress => StageMoveRequest::to_in_progress(5238, "task"),
        }
    }
}

#[test]
fn a_finished_task_moves_to_the_projects_qa_stage() {
    let (backend, server) = backend(vec![stages(), Reply::result(json!(true))]);
    let moved = backend
        .move_to_stage(&request(StageKind::Done, Some(2)))
        .expect("the move");
    assert_eq!(moved, StageMove::Moved("Quality Assurance".into()));
    assert_eq!(
        server.calls(),
        [
            ("project.task.type".into(), "search_read".into()),
            ("project.task".into(), "write".into()),
        ]
    );
}

#[test]
fn a_started_task_moves_to_the_projects_working_stage() {
    // One implementation for both directions — the Node app had two functions
    // with identical bodies and different constant lists.
    let (backend, _server) = backend(vec![stages(), Reply::result(json!(true))]);
    let moved = backend
        .move_to_stage(&request(StageKind::InProgress, Some(1)))
        .expect("the move");
    assert_eq!(moved, StageMove::Moved("In Progress".into()));
}

#[test]
fn a_project_with_no_matching_stage_is_left_alone_and_says_so() {
    // NEVER GUESS FORWARD. The next column along could be "Revision Required".
    let (backend, server) = backend(vec![Reply::result(json!([
        { "id": 1, "name": "Backlog", "sequence": 1 },
        { "id": 2, "name": "Revision Required", "sequence": 2 },
    ]))]);
    let moved = backend
        .move_to_stage(&request(StageKind::Done, Some(1)))
        .expect("an answer");
    match moved {
        StageMove::NoStage(reason) => assert!(reason.contains("QA-like"), "{reason}"),
        other => panic!("a stage was guessed: {other:?}"),
    }
    // No write went out.
    assert_eq!(
        server.calls(),
        [("project.task.type".into(), "search_read".into())]
    );
}

#[test]
fn a_task_already_in_the_target_stage_is_not_written_to() {
    let (backend, server) = backend(vec![stages()]);
    let moved = backend
        .move_to_stage(&request(StageKind::Done, Some(3)))
        .expect("an answer");
    assert_eq!(moved, StageMove::Unchanged("Quality Assurance".into()));
    assert!(
        !server
            .calls()
            .iter()
            .any(|(model, method)| model == "project.task" && method == "write"),
        "a redundant write went out"
    );
}

#[test]
fn a_task_with_no_odoo_project_is_left_alone() {
    let (backend, server) = backend(vec![stages()]);
    let moved = backend
        .move_to_stage(&StageMoveRequest {
            project_id: None,
            ..request(StageKind::Done, Some(1))
        })
        .expect("an answer");
    assert!(matches!(moved, StageMove::NoStage(_)));
    assert!(server.calls().is_empty(), "it asked Odoo anyway");
}

#[test]
fn a_project_that_turned_the_move_off_reports_disabled_rather_than_failing() {
    // The pipeline is the authority on whether the move happens at all: skip
    // the step and it stops, and that is not an error.
    let repo = tempfile::tempdir().expect("repo");
    std::fs::create_dir_all(repo.path().join(".claude-sessions")).unwrap();
    std::fs::write(
        repo.path().join(".claude-sessions/pipeline.json"),
        r#"{ "extends": "task", "steps": { "move-qa": { "skip": true } } }"#,
    )
    .unwrap();

    let (backend, server) = backend(vec![stages()]);
    let moved = backend
        .move_to_stage(&StageMoveRequest {
            repo_path: Some(repo.path().to_path_buf()),
            ..request(StageKind::Done, Some(1))
        })
        .expect("an answer");
    assert_eq!(moved, StageMove::Disabled);
    assert!(server.calls().is_empty(), "it asked Odoo anyway");
}

#[test]
fn a_project_can_name_the_stage_its_flow_moves_to() {
    // What you see under `P` is what runs.
    let repo = tempfile::tempdir().expect("repo");
    std::fs::create_dir_all(repo.path().join(".claude-sessions")).unwrap();
    std::fs::write(
        repo.path().join(".claude-sessions/pipeline.json"),
        r#"{ "extends": "task", "steps": { "move-qa": { "stage": "Staging" } } }"#,
    )
    .unwrap();

    let (backend, _server) = backend(vec![
        Reply::result(json!([
            { "id": 1, "name": "In Progress", "sequence": 1 },
            { "id": 9, "name": "Staging", "sequence": 9 },
            { "id": 3, "name": "Quality Assurance", "sequence": 3 },
        ])),
        Reply::result(json!(true)),
    ]);
    let moved = backend
        .move_to_stage(&StageMoveRequest {
            repo_path: Some(repo.path().to_path_buf()),
            ..request(StageKind::Done, Some(1))
        })
        .expect("an answer");
    // The project's own name beats the built-in QA list, which would otherwise
    // have picked "Quality Assurance".
    assert_eq!(moved, StageMove::Moved("Staging".into()));
}

#[test]
fn a_stage_the_project_named_but_does_not_have_is_reported_by_name() {
    let repo = tempfile::tempdir().expect("repo");
    std::fs::create_dir_all(repo.path().join(".claude-sessions")).unwrap();
    std::fs::write(
        repo.path().join(".claude-sessions/pipeline.json"),
        r#"{ "extends": "task", "steps": { "move-qa": { "stage": "Nowhere" } } }"#,
    )
    .unwrap();

    let (backend, _server) = backend(vec![Reply::result(json!([
        { "id": 1, "name": "Backlog", "sequence": 1 },
    ]))]);
    let moved = backend
        .move_to_stage(&StageMoveRequest {
            repo_path: Some(repo.path().to_path_buf()),
            ..request(StageKind::Done, Some(1))
        })
        .expect("an answer");
    match moved {
        StageMove::NoStage(reason) => assert!(reason.contains("\"Nowhere\""), "{reason}"),
        other => panic!("expected a named miss, got {other:?}"),
    }
}

#[test]
fn a_configured_override_beats_the_built_in_names() {
    let (backend, _server) = backend(vec![
        Reply::result(json!([
            { "id": 3, "name": "Quality Assurance", "sequence": 3 },
            { "id": 7, "name": "UAT", "sequence": 7 },
        ])),
        Reply::result(json!(true)),
    ]);
    let moved = backend
        .move_to_stage(&StageMoveRequest {
            preferred: vec!["UAT".into()],
            ..request(StageKind::Done, Some(1))
        })
        .expect("an answer");
    assert_eq!(moved, StageMove::Moved("UAT".into()));
}

#[test]
fn an_odoo_failure_is_reported_rather_than_swallowed() {
    let (backend, _server) = backend(vec![Reply::error("Access Denied")]);
    let error = backend
        .move_to_stage(&request(StageKind::Done, Some(1)))
        .expect_err("an error");
    assert!(error.contains("Access Denied"), "{error}");
}

#[test]
fn task_detail_carries_the_ids_the_stage_move_needs() {
    let (backend, _server) = backend(vec![Reply::result(json!([{
        "id": 5238,
        "name": "Report templates",
        "description": "<p>body</p>",
        "stage_id": [2, "In Progress"],
        "project_id": [3, "Repo"],
        "priority": "0",
        "date_deadline": false,
    }]))]);
    let detail = backend.task_detail(5238).expect("a detail");
    assert_eq!(detail.project_id, Some(3));
    assert_eq!(detail.stage_id, Some(2));
    assert_eq!(detail.name, "Report templates");
    assert_eq!(detail.project_name, "Repo");
}

#[test]
fn a_task_odoo_does_not_have_is_an_error_not_an_empty_detail() {
    let (backend, _server) = backend(vec![Reply::result(json!([]))]);
    let error = backend.task_detail(5238).expect_err("an error");
    assert!(error.contains("#5238"), "{error}");
}

#[test]
fn the_completion_comment_reaches_the_tasks_chatter_as_html() {
    let (backend, server) = backend(vec![Reply::result(json!(1))]);
    backend.post_comment(5238, "<p>done</p>").expect("posted");
    let args = server.args(1);
    assert_eq!(args[4], json!("message_post"));
    assert_eq!(args[6]["body"], json!("<p>done</p>"));
    // Without this Odoo renders the markup as literal text.
    assert_eq!(args[6]["body_is_html"], json!(true));
}
