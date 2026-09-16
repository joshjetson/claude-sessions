//! Authentication, the `execute_kw` envelope, error shapes and the timeout.

use std::time::Duration;

use serde_json::json;

use super::stub::{Reply, StubServer};
use super::{client_with, creds};
use crate::odoo::{HttpTransport, OdooClient, OdooError};

#[test]
fn authenticate_sends_the_common_service_envelope() {
    let (client, server) = client_with(vec![]);
    assert_eq!(client.authenticate().unwrap(), 7);

    let request = &server.requests()[0];
    assert_eq!(request["jsonrpc"], json!("2.0"));
    assert_eq!(request["method"], json!("call"));
    assert_eq!(request["id"], json!(1));
    assert_eq!(request["params"]["service"], json!("common"));
    assert_eq!(request["params"]["method"], json!("authenticate"));
    assert_eq!(
        server.args(0),
        vec![json!("testdb"), json!("tester"), json!("secret"), json!({})]
    );
}

#[test]
fn the_uid_is_cached_until_it_is_invalidated() {
    let (client, server) = client_with(vec![]);
    client.authenticate().unwrap();
    client.authenticate().unwrap();
    assert_eq!(server.request_count(), 1, "the uid was fetched twice");

    // Node cached the uid for the life of the process with no way back; an
    // expired session then failed every call until the dashboard restarted.
    client.invalidate_auth();
    client.authenticate().unwrap();
    assert_eq!(server.request_count(), 2);
}

#[test]
fn execute_kw_carries_db_uid_password_model_method_args_and_kwargs() {
    let (client, server) = client_with(vec![Reply::result(json!([{ "id": 4033 }]))]);
    client
        .execute_kw(
            "project.task",
            "read",
            vec![json!([4033])],
            json!({ "fields": ["id"] }),
        )
        .unwrap();

    let request = &server.requests()[1];
    assert_eq!(request["params"]["service"], json!("object"));
    assert_eq!(request["params"]["method"], json!("execute_kw"));
    assert_eq!(
        server.args(1),
        vec![
            json!("testdb"),
            json!(7),
            json!("secret"),
            json!("project.task"),
            json!("read"),
            json!([[4033]]),
            json!({ "fields": ["id"] }),
        ]
    );
}

#[test]
fn an_odoo_error_object_surfaces_its_inner_message() {
    let (client, _server) = client_with(vec![Reply::error(
        "Record does not exist or has been deleted.",
    )]);
    let err = client
        .execute_kw("project.task", "read", vec![json!([1])], json!({}))
        .unwrap_err();
    assert_eq!(
        err,
        OdooError::Rpc("Record does not exist or has been deleted.".to_string())
    );
    assert!(!err.is_auth(), "an RPC error must not drop the cached uid");
}

#[test]
fn an_error_without_data_falls_back_to_the_outer_message() {
    let (client, _server) = client_with(vec![Reply::raw(
        200,
        json!({ "error": { "message": "Access Denied" } }).to_string(),
    )]);
    let err = client
        .execute_kw("project.task", "read", vec![json!([1])], json!({}))
        .unwrap_err();
    assert_eq!(err, OdooError::Rpc("Access Denied".to_string()));
}

#[test]
fn a_rejected_login_says_what_the_process_saw() {
    let server = StubServer::start(vec![Reply::result(json!(false))]);
    let client = OdooClient::new(creds(&server.url()));
    let err = client.authenticate().unwrap_err();
    match &err {
        OdooError::AuthRejected(detail) => {
            assert!(detail.contains("user=tester"), "{detail}");
            assert!(detail.contains("db=testdb"), "{detail}");
            assert!(detail.contains("pw=6 chars"), "{detail}");
        }
        other => panic!("expected AuthRejected, got {other:?}"),
    }
    assert!(err.is_auth());
}

#[test]
fn incomplete_credentials_are_reported_before_any_request() {
    let server = StubServer::start(vec![Reply::result(json!(7))]);
    let mut partial = creds(&server.url());
    partial.db = String::new();
    partial.password = String::new();
    let client = OdooClient::new(partial);

    assert!(!client.has_credentials());
    let err = client.authenticate().unwrap_err();
    assert_eq!(err, OdooError::MissingCredentials(vec!["db", "password"]));
    assert_eq!(server.request_count(), 0, "a request went out anyway");
    assert!(err.to_string().contains("ODOO_* env"), "{err}");
}

#[test]
fn a_body_that_is_not_json_is_reported_with_what_arrived() {
    let (client, _server) = client_with(vec![Reply::raw(200, "<html>login</html>")]);
    let err = client
        .execute_kw("project.task", "read", vec![], json!({}))
        .unwrap_err();
    match err {
        OdooError::BadResponse(detail) => assert!(detail.contains("<html>login"), "{detail}"),
        other => panic!("expected BadResponse, got {other:?}"),
    }
}

#[test]
fn a_hung_server_times_out_instead_of_freezing_the_refresh() {
    // The Node client passed no timeout to fetch, so a VPN that dropped while
    // the dashboard was open wedged the refresh until the process was killed.
    let server = StubServer::start(vec![
        Reply::result(json!(7)).after(Duration::from_millis(400))
    ]);
    let client = OdooClient::with_transport(
        creds(&server.url()),
        Box::new(HttpTransport::new(Duration::from_millis(80))),
    );
    let err = client.authenticate().unwrap_err();
    assert!(
        matches!(err, OdooError::Transport(_)),
        "expected a transport error, got {err:?}"
    );
}

#[test]
fn task_url_opens_the_form_view() {
    let client = OdooClient::new(creds("https://odoo.example"));
    assert_eq!(
        client.task_url(5944),
        "https://odoo.example/web#id=5944&model=project.task&view_type=form"
    );
}
