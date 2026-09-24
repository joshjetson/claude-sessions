//! The client half: find a daemon, subscribe to its stream, post actions to it.
//!
//! Hand-written HTTP over [`TcpStream`], for the same reason the server is:
//! the whole conversation is loopback JSON plus one long-lived stream, and the
//! only hard part — reassembling SSE frames that arrive split across arbitrary
//! chunk boundaries — is ours either way. [`SseParser`] is public so that
//! reassembly can be driven with adversarial splits in a test rather than
//! hoped about.

use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Value};

use crate::types::NotificationStatus;

use super::protocol::{read_body, read_head};
use super::{BoardFilter, PendingRequest, RefreshRequest, Snapshot};

mod discovery;
mod sse;

pub use discovery::{ensure_daemon, probe, spawn_daemon, DaemonTarget, Health};
pub use sse::{subscribe, SseEvent, SseMessage, SseParser, Subscription};
#[cfg(test)]
pub(crate) use sse::{subscribe_with, HANDSHAKE_TIMEOUT, READ_TIMEOUT};

/// How long a health probe waits. Short: the answer decides whether the
/// dashboard starts remote or embedded, and a hung probe is a hung startup.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Reads.
const GET_TIMEOUT: Duration = Duration::from_secs(5);
/// Writes, which may wait on the engine.
const POST_TIMEOUT: Duration = Duration::from_secs(10);
/// Reconnect backoff bounds for the event stream.
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Autostart: how long to wait for a freshly spawned daemon, and how often to
/// ask whether it is up yet.
pub(super) const AUTOSTART_DEADLINE: Duration = Duration::from_secs(5);
pub(super) const AUTOSTART_POLL: Duration = Duration::from_millis(150);
pub(super) const AUTOSTART_PROBE: Duration = Duration::from_millis(400);
/// Largest response body accepted from the daemon.
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// A parsed HTTP response. `raw` is kept alongside `body` so a caller (and a
/// test) can assert on exactly what came over the socket rather than on a
/// re-serialisation of it.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub raw: String,
    pub body: Value,
}

impl Response {
    /// The `ok` field the action routes answer with.
    pub fn accepted(&self) -> bool {
        self.body
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// One request/response round trip. The daemon closes the connection after
/// answering, so the body is whatever arrives before EOF or `Content-Length`,
/// whichever comes first.
///
/// Public because the typed wrappers below cannot cover everything a caller
/// might want to ask — a status code from an unknown route, or a deliberately
/// malformed body.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Option<Response> {
    let mut stream = TcpStream::connect_timeout(&loopback(port), timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let payload = body.unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).ok()?;
    stream.write_all(payload.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader).ok()??;
    let status = head.parts().1.parse().ok()?;
    let length = head.content_length();
    let raw = if length > 0 {
        read_body(&mut reader, length, MAX_RESPONSE).ok()?
    } else {
        let mut raw = Vec::new();
        reader
            .take(MAX_RESPONSE as u64)
            .read_to_end(&mut raw)
            .ok()?;
        raw
    };
    let raw = String::from_utf8_lossy(&raw).into_owned();
    Some(Response {
        status,
        body: serde_json::from_str(&raw).unwrap_or(Value::Null),
        raw,
    })
}

// --- actions ----------------------------------------------------------------

/// The thin wrappers over the action routes. One per route, so a caller never
/// writes a path.
#[derive(Debug, Clone, Copy)]
pub struct DaemonClient {
    port: u16,
}

impl DaemonClient {
    pub fn new(port: u16) -> Self {
        DaemonClient { port }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn get(&self, path: &str) -> Option<Response> {
        request(self.port, "GET", path, None, GET_TIMEOUT)
    }

    fn post(&self, path: &str, body: Value) -> Option<Response> {
        request(
            self.port,
            "POST",
            path,
            Some(&body.to_string()),
            POST_TIMEOUT,
        )
    }

    pub fn health(&self) -> Option<Health> {
        probe(self.port, PROBE_TIMEOUT)
    }

    pub fn state(&self) -> Option<Snapshot> {
        serde_json::from_value(self.get("/state")?.body).ok()
    }

    pub fn refresh(&self, request: RefreshRequest) -> bool {
        self.post(
            "/refresh",
            json!({
                "forceDiscovery": request.force_discovery,
                "board": request.board,
                "deploy": request.deploy,
            }),
        )
        .is_some_and(|response| response.accepted())
    }

    pub fn refresh_usage(&self) -> bool {
        self.post("/usage/refresh", json!({}))
            .is_some_and(|response| response.accepted())
    }

    pub fn set_board_filter(&self, filter: BoardFilter) -> bool {
        self.post("/board/filter", json!({ "filter": filter.as_str() }))
            .is_some_and(|response| response.accepted())
    }

    pub fn set_pending(&self, pending: &PendingRequest) -> bool {
        self.post(
            "/session/pending",
            json!({
                "cwd": pending.cwd,
                "taskId": pending.task_id,
                "knownSessionIds": pending.known_session_ids,
            }),
        )
        .is_some_and(|response| response.accepted())
    }

    pub fn link_task(&self, task_id: i64, info: Value) -> bool {
        self.post("/task/link", json!({ "taskId": task_id, "info": info }))
            .is_some_and(|response| response.accepted())
    }

    pub fn set_notification_status(&self, ids: &[String], status: NotificationStatus) -> bool {
        self.post(
            "/notifications/status",
            json!({ "ids": ids, "status": status.as_str() }),
        )
        .is_some_and(|response| response.accepted())
    }

    pub fn dismiss_notifications(&self, ids: &[String]) -> bool {
        self.post("/notifications/dismiss", json!({ "ids": ids }))
            .is_some_and(|response| response.accepted())
    }

    /// Resolve every notification in the daemon's feed.
    pub fn clear_notifications(&self) -> bool {
        self.post("/notifications/clear", json!({}))
            .is_some_and(|response| response.accepted())
    }

    /// The legacy notification contract `claude-sessions notify` posts.
    pub fn notify(&self, body: Value) -> Option<Response> {
        self.post("/notify", body)
    }

    /// A coordinator answering one of the sessions in its run.
    ///
    /// Goes through the daemon rather than the coordinator driving a terminal
    /// itself, because the daemon is what actually knows which session is on
    /// which task — and because the rule about what may be answered then lives
    /// in one module instead of in a prompt a model could talk itself out of.
    pub fn qa_answer(&self, body: Value) -> Option<Response> {
        self.post("/qa-run/answer", body)
    }

    pub fn done(&self, task_id: i64, cwd: &str, summary: &str) -> bool {
        self.post(
            "/done",
            json!({ "taskId": task_id, "cwd": cwd, "summary": summary }),
        )
        .is_some_and(|response| response.accepted())
    }

    pub fn blocked(&self, task_id: i64, cwd: &str, questions: &[String]) -> bool {
        self.post(
            "/blocked",
            json!({ "taskId": task_id, "cwd": cwd, "questions": questions }),
        )
        .is_some_and(|response| response.accepted())
    }

    /// Start a deploy on the daemon, so it outlives this dashboard.
    ///
    /// The refusal (409) is returned rather than swallowed: "already
    /// deploying" and "no command configured" are both things the user needs
    /// to be told, in the daemon's own words.
    pub fn start_deploy(&self, project: &str) -> Option<Response> {
        self.post("/deploy/start", json!({ "project": project }))
    }

    pub fn cancel_deploy(&self, project: &str) -> Option<Response> {
        self.post("/deploy/cancel", json!({ "project": project }))
    }

    /// Everything the daemon still holds for a run — the whole ring buffer,
    /// not the trailing window the events carry.
    pub fn deploy_log(&self, project: &str) -> Vec<String> {
        let encoded = crate::gitlab::encode_uri_component(project);
        self.get(&format!("/deploy/log/{encoded}"))
            .and_then(|response| serde_json::from_value(response.body.get("lines")?.clone()).ok())
            .unwrap_or_default()
    }

    /// Ask the daemon to reload the deploy board. Manual by design: every
    /// refresh spends a GitLab API call per open merge request.
    pub fn refresh_deploy(&self) -> bool {
        self.refresh(RefreshRequest {
            deploy: true,
            ..RefreshRequest::default()
        })
    }

    pub fn shutdown(&self) -> bool {
        self.post("/shutdown", json!({}))
            .is_some_and(|response| response.accepted())
    }
}
