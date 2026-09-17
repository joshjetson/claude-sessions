//! Ported from the `HTTP surface` and `notification status` blocks of
//! `test/daemon.test.js`.
//!
//! Every server here binds port 0 — the kernel picks an unused one — so the
//! suite can never disturb a real daemon on 8787 and two of these can run at
//! the same time. The engine is built but never started, exactly as the Node
//! suite did it: these tests are the protocol, not the pollers.

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use super::fakes::FakeProcesses;
use super::*;
use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::client::{self, Response};
use crate::daemon::server::{self, ServerHandle};
use crate::daemon::{Engine, EngineOptions, NewNotification};
use crate::db::Db;
use crate::scan::{Discovery, Scanner};
use crate::term::SpawnPolicy;
use crate::types::NotificationStatus;

/// A daemon serving a throwaway engine on an ephemeral port.
/// Fields drop in declaration order, so the server stops — and its threads
/// join — before the temp tree it is serving is deleted.
pub(crate) struct Served {
    pub(crate) server: ServerHandle,
    pub(crate) engine: Arc<Engine<FakeProcesses>>,
    pub(crate) paths: Paths,
    _dir: TempDir,
}

impl Served {
    pub(crate) fn port(&self) -> u16 {
        self.server.port()
    }

    pub(crate) fn client(&self) -> crate::daemon::DaemonClient {
        crate::daemon::DaemonClient::new(self.port())
    }

    pub(crate) fn db(&self) -> &Db {
        self.engine.db()
    }
}

pub(crate) fn served() -> Served {
    served_with(None, None)
}

/// The same, with the outside world wired in — a daemon that can fetch a board
/// and a deploy list when it is asked to.
pub(crate) fn served_with(
    board: Option<crate::daemon::BoardFetch>,
    deploy: Option<crate::daemon::DeployFetch>,
) -> Served {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).unwrap();
    fs::create_dir_all(&paths.projects_dir).unwrap();
    let config = ConfigHandle::load_from(&paths.config_path, &paths.home, EnvOverrides::default());
    let engine = Arc::new(Engine::new(EngineOptions {
        spawn: SpawnPolicy::Refuse,
        fetch_board: board,
        fetch_deploy: deploy,
        ..EngineOptions::with_scanner(
            paths.clone(),
            config,
            Scanner::new(FakeProcesses::new(), paths.clone(), Discovery::Processes),
        )
    }));
    // Port 0: never a fixed one. A suite that pinned a port would fight the
    // user's own daemon the moment it ran on a working machine.
    let listener = server::bind(0).expect("could not bind an ephemeral port");
    let server = server::serve(Arc::clone(&engine), listener).unwrap();
    Served {
        server,
        engine,
        paths,
        _dir: dir,
    }
}

// --- helpers ----------------------------------------------------------------

pub(crate) const PATIENCE: Duration = Duration::from_secs(5);

/// Poll until something becomes true, or fail the test saying what never did.
pub(crate) fn wait_for(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {label}");
}

pub(crate) fn get(port: u16, path: &str) -> Response {
    client::request(port, "GET", path, None, PATIENCE).expect("no response to a GET")
}

pub(crate) fn post(port: u16, path: &str, body: &str) -> Response {
    client::request(port, "POST", path, Some(body), PATIENCE).expect("no response to a POST")
}

fn info(title: &str) -> NewNotification {
    NewNotification::new("test", title, "body")
}

// --- isolation --------------------------------------------------------------

#[test]
fn every_server_here_is_isolated_from_the_real_runtime_directory() {
    // The Node suite's regression guard, and it earned its place: these tests
    // drive a real engine, and `pushNotification` persists — without isolation
    // the test notifications land in the user's actual feed and the test server
    // rewrites the port file that `claude-sessions notify` reads.
    let served = served();
    assert!(served.paths.is_isolated());
    assert!(served.paths.db_path.starts_with(&served.paths.runtime_dir));
    assert_ne!(
        served.paths.port_file,
        served
            .paths
            .home
            .join(".claude-sessions")
            .join("notify.json"),
    );
    assert_ne!(
        served.port(),
        crate::daemon::DEFAULT_PORT,
        "an ephemeral port can never be the one a real daemon owns"
    );
}

// --- health, state, 404s ----------------------------------------------------

#[test]
fn health_identifies_a_real_daemon() {
    let served = served();
    let health = client::probe(served.port(), PATIENCE).expect("no daemon answered");
    assert!(health.ok);
    assert!(
        health.daemon,
        "a client must be able to tell a daemon apart"
    );
    assert_eq!(health.pid, std::process::id());
    assert_eq!(health.clients, 0);
}

#[test]
fn state_returns_a_complete_snapshot() {
    let served = served();
    let body = get(served.port(), "/state").body;
    for key in [
        "sessions",
        "taskSessions",
        "archivedTasks",
        "doneTasks",
        "blockedTasks",
        "notifications",
        "boardFilter",
    ] {
        assert!(body.get(key).is_some(), "snapshot is missing {key}: {body}");
    }
    assert!(body["sessions"].get("byProject").is_some());
    assert!(body["sessions"]["stats"]["totalSessions"].is_number());
}

#[test]
fn unknown_routes_404_as_json_not_html() {
    let served = served();
    let response = get(served.port(), "/nope");
    assert_eq!(response.status, 404);
    assert_eq!(response.body["ok"], json!(false));
    assert!(
        response.raw.trim_start().starts_with('{'),
        "a client parsing JSON would choke on {}",
        response.raw
    );

    // A known path with the wrong method is just as unknown.
    assert_eq!(get(served.port(), "/notify").status, 404);
    assert_eq!(post(served.port(), "/state", "{}").status, 404);
}

// --- the legacy /notify contract --------------------------------------------

#[test]
fn notify_keeps_the_contract_the_helper_cli_depends_on() {
    let served = served();
    let response = post(
        served.port(),
        "/notify",
        &json!({
            "title": "hello",
            "message": "world",
            "level": "warn",
            "cwd": "/Users/x/dev/thing",
        })
        .to_string(),
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.body["ok"], json!(true));
    assert!(response.body["id"].is_string(), "{}", response.raw);

    let state = served.engine.snapshot();
    let raised = &state.notifications[0];
    assert_eq!(raised.title, "hello");
    assert_eq!(raised.message, "world");
    assert_eq!(raised.level.as_str(), "warn");
    assert_eq!(raised.status, NotificationStatus::Unread);
    assert_eq!(
        raised.project, "x/thing",
        "project must be derived from cwd"
    );
}

#[test]
fn oversized_notification_fields_are_clamped() {
    let served = served();
    post(
        served.port(),
        "/notify",
        &json!({ "title": "T".repeat(500), "message": "M".repeat(9000) }).to_string(),
    );
    let raised = served.engine.snapshot().notifications[0].clone();
    assert_eq!(raised.title.chars().count(), 200);
    assert_eq!(raised.message.chars().count(), 4000);
}

#[test]
fn malformed_json_bodies_are_tolerated_not_fatal() {
    // An agent posts this from a shell one-liner inside its own transcript; a
    // quoting mistake must still raise something rather than vanish.
    let served = served();
    let response = post(served.port(), "/notify", "{not json");
    assert_eq!(response.status, 200);
    assert_eq!(
        served.engine.snapshot().notifications[0].title,
        "Notification"
    );
}

#[test]
fn a_task_id_arriving_as_text_is_still_a_task_id() {
    let served = served();
    post(
        served.port(),
        "/notify",
        &json!({ "title": "t", "taskId": "4242" }).to_string(),
    );
    assert_eq!(
        served.engine.snapshot().notifications[0].task_id,
        Some(4242),
        "the helper CLI reads it off a command line, where everything is text"
    );
}

// --- notification status ----------------------------------------------------

#[test]
fn a_status_change_reaches_the_engine_the_database_and_every_client() {
    let served = served();
    let raised = served.engine.push_notification(info("r1"));
    let (events, mut subscription) = collect(served.port());
    wait_for("the snapshot", || {
        events.lock().unwrap().iter().any(|e| e.event == "snapshot")
    });

    let response = post(
        served.port(),
        "/notifications/status",
        &json!({ "ids": [raised.id.clone()], "status": "resolved" }).to_string(),
    );
    assert_eq!(response.status, 202);

    assert_eq!(
        served.engine.snapshot().notifications[0].status,
        NotificationStatus::Resolved
    );
    assert_eq!(
        served.db().recent_notifications(10)[0].status,
        NotificationStatus::Resolved,
        "the change never reached SQLite — it would come back on restart"
    );
    wait_for("the notifications-changed broadcast", || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.event == "notifications-changed" && e.data.contains("resolved"))
    });
    subscription.close();
}

#[test]
fn an_empty_id_list_is_rejected_rather_than_updating_everything() {
    let served = served();
    served.engine.push_notification(info("safe1"));
    let response = post(
        served.port(),
        "/notifications/status",
        &json!({ "ids": [], "status": "resolved" }).to_string(),
    );
    assert_eq!(response.status, 400);
    assert_eq!(response.body["ok"], json!(false));
    assert_eq!(
        served.engine.snapshot().notifications[0].status,
        NotificationStatus::Unread
    );
}

#[test]
fn dismiss_removes_it_from_the_engine_and_the_database() {
    let served = served();
    let raised = served.engine.push_notification(info("d1"));
    assert!(served
        .client()
        .dismiss_notifications(std::slice::from_ref(&raised.id)));
    assert!(served.engine.snapshot().notifications.is_empty());
    assert!(served.db().recent_notifications(10).is_empty());
    assert_eq!(
        post(served.port(), "/notifications/dismiss", "{}").status,
        400
    );
}

/// Subscribe and collect every event, for the tests that assert on broadcasts.
pub(crate) fn collect(
    port: u16,
) -> (
    Arc<std::sync::Mutex<Vec<crate::daemon::SseEvent>>>,
    crate::daemon::Subscription,
) {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let subscription = client::subscribe(port, move |message| {
        if let crate::daemon::SseMessage::Event(event) = message {
            sink.lock().unwrap().push(event);
        }
    });
    (events, subscription)
}

/// A JSON value out of an event's data, for readability in assertions.
pub(crate) fn data(event: &crate::daemon::SseEvent) -> Value {
    event.json().unwrap_or(Value::Null)
}
