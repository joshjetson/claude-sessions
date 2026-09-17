//! The action routes, the deploy refusals, and what actually crosses the wire.
//!
//! The second half of `test/daemon.test.js`'s HTTP coverage; the harness lives
//! next door in `server.rs` because these drive the same kind of server.

use std::io::Write;
use std::net::TcpStream;

use serde_json::json;

use super::server::{collect, get, post, served, wait_for, PATIENCE};
use super::*;
use crate::daemon::client;
use crate::daemon::server::{over_backlog, MAX_CLIENT_BACKLOG};
use crate::daemon::NewNotification;
use crate::types::Prompt;

fn info(title: &str) -> NewNotification {
    NewNotification::new("test", title, "body")
}

// --- the other action routes ------------------------------------------------

#[test]
fn a_launch_and_a_link_reach_the_engine() {
    let served = served();
    assert!(served.client().set_pending(&crate::daemon::PendingRequest {
        cwd: "/tmp/repo".to_string(),
        task_id: Some(4242),
        known_session_ids: vec!["old".to_string()],
    }));
    assert!(served.client().link_task(
        4242,
        json!({ "cwd": "/tmp/repo", "sessionId": "sess-abc", "status": "running" }),
    ));

    let snapshot = served.engine.snapshot();
    let link = &snapshot.task_sessions[&4242];
    assert_eq!(link.cwd, "/tmp/repo");
    assert_eq!(link.session_id, "sess-abc");
    assert!(link.is_running());
}

#[test]
fn a_refresh_runs_a_tick_and_a_filter_is_stored() {
    let served = served();
    assert!(served
        .client()
        .refresh(crate::daemon::RefreshRequest::discovery()));
    assert!(served.engine.stats().refreshes >= 1);

    assert!(served
        .client()
        .set_board_filter(crate::daemon::BoardFilter::All));
    assert_eq!(served.engine.snapshot().board_filter, "all");
    // An unrecognised filter leaves the board as it was rather than 400ing:
    // this is user input arriving over HTTP.
    assert_eq!(
        post(
            served.port(),
            "/board/filter",
            &json!({ "filter": "nonsense" }).to_string()
        )
        .status,
        202
    );
    assert_eq!(served.engine.snapshot().board_filter, "all");
}

#[test]
fn done_and_blocked_are_accepted_and_handled_off_the_request() {
    // Fire and forget: the route answers at once because the work behind it
    // takes seconds of Odoo and `glab` round trips.
    let served = served();
    assert!(served.client().done(4242, "/tmp/repo", "fixed it"));
    assert!(served
        .client()
        .blocked(4243, "/tmp/repo", &["which key?".to_string()]));
    served.engine.join_workers();
    assert!(served.engine.snapshot().blocked_tasks.contains_key(&4243));
}

#[test]
fn a_deploy_route_refuses_with_409_and_names_the_reason() {
    // 409, never 400: the request was well formed. "Not configured" and
    // "already deploying" are states the caller can see and fix, and the
    // sentence is what the Deploy tab shows.
    let served = served();
    let unknown = post(
        served.port(),
        "/deploy/start",
        &json!({ "project": "Nowhere" }).to_string(),
    );
    assert_eq!(unknown.status, 409);
    assert_eq!(unknown.body["ok"], json!(false));
    assert!(unknown.body["error"]
        .as_str()
        .unwrap()
        .contains("not configured for deploy"));

    let idle = post(
        served.port(),
        "/deploy/cancel",
        &json!({ "project": "Nowhere" }).to_string(),
    );
    assert_eq!(idle.status, 409);
    assert_eq!(idle.body["error"], json!("No running deploy for Nowhere."));
}

#[test]
fn starting_a_deploy_twice_is_a_409_and_the_log_route_reads_the_buffer() {
    let served = served();
    // A run put there directly: starting a real one would spawn a process, and
    // the server suite runs under a refusing policy like everything else.
    served.engine.inner().state().deploy_runs.insert(
        "Project A".to_string(),
        crate::daemon::DeployRunState::started("./deploy.sh", Some(1)),
    );
    served
        .engine
        .inner()
        .push_deploy_line("Project A", "building".to_string());

    let again = post(
        served.port(),
        "/deploy/start",
        &json!({ "project": "Project A" }).to_string(),
    );
    assert_eq!(again.status, 409);

    // Percent-decoded, and answering with the whole buffer.
    let log = get(served.port(), "/deploy/log/Project%20A");
    assert_eq!(log.status, 200);
    assert_eq!(log.body["project"], json!("Project A"));
    assert_eq!(log.body["lines"], json!(["building"]));

    // A project nobody deployed is an empty log, not a 404.
    let empty = get(served.port(), "/deploy/log/Nowhere");
    assert_eq!(empty.status, 200);
    assert_eq!(empty.body["lines"], json!([]));
}

#[test]
fn cancelling_a_running_deploy_is_accepted() {
    let served = served();
    served.engine.inner().state().deploy_runs.insert(
        "Project A".to_string(),
        crate::daemon::DeployRunState::started("./deploy.sh", Some(1)),
    );
    let response = post(
        served.port(),
        "/deploy/cancel",
        &json!({ "project": "Project A" }).to_string(),
    );
    // Accepted: the signal is sent (and refused by the spawn policy under
    // test), and the run's own exit is what reports the outcome.
    assert_eq!(response.status, 202);
    assert_eq!(response.body["ok"], json!(true));
}

#[test]
fn shutdown_answers_first_and_stops_the_server_after() {
    let served = served();
    let response = post(served.port(), "/shutdown", "{}");
    assert_eq!(response.status, 200);
    assert_eq!(response.body["ok"], json!(true));
    assert!(
        served.server.wait_for_shutdown(PATIENCE),
        "the server never stopped"
    );
}

// --- what crosses the wire --------------------------------------------------

#[test]
fn the_heavy_transcript_fields_never_reach_a_client() {
    // Through a real socket, not through `wire_session`: the assertion is about
    // the bytes a client actually receives.
    let served = served();
    served.engine.snapshot();
    {
        let mut state = served.engine.inner().state();
        state.sessions = index(vec![Session {
            last_entry: Some(tool_entry("Bash")),
            prompts: vec![Prompt {
                text: "a".to_string(),
                timestamp: "t".to_string(),
            }],
            cumulative_usage: Some(crate::types::CumulativeUsage {
                input_tokens: 1,
                ..crate::types::CumulativeUsage::default()
            }),
            ..a_session("s1", "/Users/someone/dev/repo")
        }]);
    }

    let raw = get(served.port(), "/state").raw;
    for key in ["lastEntry", "prompts", "cumulativeUsage"] {
        assert!(!raw.contains(key), "{key} crossed the boundary: {raw}");
    }
    assert!(raw.contains("\"s1\""), "the session itself never arrived");
}

#[test]
fn a_client_too_far_behind_is_dropped_rather_than_buffered() {
    // The rule, not a simulated slow reader: a client that cannot keep up costs
    // the daemon four megabytes at most, then loses its stream.
    assert!(!over_backlog(0, 1024));
    assert!(!over_backlog(MAX_CLIENT_BACKLOG - 1, 1));
    assert!(over_backlog(MAX_CLIENT_BACKLOG, 1));
    assert!(over_backlog(usize::MAX, 1), "the addition must not wrap");
}

#[test]
fn the_client_count_follows_connections() {
    let served = served();
    let (_events, mut subscription) = collect(served.port());
    wait_for("the client to register", || {
        served.server.client_count() == 1
    });
    assert_eq!(
        client::probe(served.port(), PATIENCE).unwrap().clients,
        1,
        "/health must report what is actually connected"
    );
    subscription.close();
    wait_for("the client to be forgotten", || {
        served.server.client_count() == 0
    });
}

#[test]
fn a_client_that_vanishes_mid_stream_is_forgotten() {
    // The bug this guards: Node wrote to an ended stream, which raised an
    // asynchronous error that took the whole daemon down.
    let served = served();
    let mut socket = TcpStream::connect(("127.0.0.1", served.port())).unwrap();
    let request = format!(
        "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
        served.port()
    );
    socket.write_all(request.as_bytes()).unwrap();
    wait_for("the client to register", || {
        served.server.client_count() == 1
    });
    drop(socket);

    // Keep publishing: the write that fails is what notices, and it must take
    // only that client with it.
    wait_for("the dead client to be dropped", || {
        served
            .engine
            .push_notification(info("after the client left"));
        served.server.client_count() == 0
    });
    assert!(
        client::probe(served.port(), PATIENCE).is_some(),
        "the daemon died with its client"
    );
}
