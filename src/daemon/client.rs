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
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths::Paths;
use crate::types::NotificationStatus;

use super::protocol::{read_body, read_head};
use super::{BoardFilter, PendingRequest, RefreshRequest, Snapshot};

mod sse;

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
const AUTOSTART_DEADLINE: Duration = Duration::from_secs(5);
const AUTOSTART_POLL: Duration = Duration::from_millis(150);
const AUTOSTART_PROBE: Duration = Duration::from_millis(400);
/// Largest response body accepted from the daemon.
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// What `/health` answers.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Health {
    pub ok: bool,
    pub daemon: bool,
    /// Which program is on the port. Empty for anything that predates the
    /// marker, which includes the Node tool this one replaced.
    #[serde(rename = "impl")]
    pub implementation: String,
    /// Its version, for a person to read. Never used to accept or refuse.
    pub version: String,
    pub pid: u32,
    pub uptime: f64,
    pub clients: usize,
}

impl Health {
    /// Whether the daemon on the other end is this program.
    ///
    /// Anything else — the Node tool, a build older than the marker, a
    /// stranger on the port that happens to answer `/health` — is not
    /// something to mirror: its event stream means nothing here, and
    /// subscribing to it produces an empty dashboard and no error.
    pub fn is_this_implementation(&self) -> bool {
        self.implementation == super::protocol::IMPLEMENTATION
    }

    /// Whatever is on the port, in words, for a status line or a notice.
    pub fn describe(&self) -> String {
        if self.is_this_implementation() {
            let version = match self.version.as_str() {
                "" => "an unnamed version".to_string(),
                version => format!("v{version}"),
            };
            return format!("claude-sessions {version}");
        }
        match self.implementation.as_str() {
            "" => {
                "an unmarked daemon (an older claude-sessions, or the Node tool of the same name)"
                    .to_string()
            }
            other => format!("a different implementation ({other})"),
        }
    }
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

/// Is a daemon answering on this port? Its `/health` payload, or `None`.
pub fn probe(port: u16, timeout: Duration) -> Option<Health> {
    let response = request(port, "GET", "/health", None, timeout)?;
    let health: Health = serde_json::from_value(response.body).ok()?;
    health.ok.then_some(health)
}

/// A daemon to talk to.
#[derive(Debug, Clone)]
pub struct DaemonTarget {
    pub port: u16,
    pub health: Health,
    /// True when this call is what started it.
    pub spawned: bool,
}

/// Get a daemon: the one already running, or a freshly started one when
/// autostart is enabled.
///
/// The error is a finished sentence rather than a code, because there is
/// exactly one thing to do with it: show it. Falling back to an in-process
/// scan is the right behaviour and always was — doing it without a word was
/// the bug, because "the daemon never came up" and "there are no sessions"
/// look identical on screen.
pub fn ensure_daemon(paths: &Paths, port: u16, autostart: bool) -> Result<DaemonTarget, String> {
    if let Some(health) = probe(port, PROBE_TIMEOUT) {
        // Answering is not the same as being ours. Mirroring a daemon whose
        // events this build cannot read is the failure that looks most like
        // success: connected, subscribed, and empty forever.
        if !health.is_this_implementation() {
            return Err(foreign_daemon(port, &health));
        }
        return Ok(DaemonTarget {
            port,
            health,
            spawned: false,
        });
    }
    if !autostart {
        return Err(format!(
            "No daemon on :{port}, and daemon.autostart is off."
        ));
    }
    spawn_daemon(paths, port).map(|health| DaemonTarget {
        port,
        health,
        spawned: true,
    })
}

/// One sentence for a port held by something this build cannot talk to.
///
/// Written once because it is reached from both ends of the same question —
/// before a spawn and after one — and the two must not drift.
fn foreign_daemon(port: u16, health: &Health) -> String {
    format!(
        "Port {port} is held by {} — its sessions cannot be read here. \
         Stop it, or give this one its own `daemon.port`.",
        health.describe()
    )
}

/// Start `claude-sessions daemon` detached and wait for it to answer.
///
/// Detached is the whole point: the daemon owns the board polling and the
/// deploys, so it has to outlive the dashboard that started it — including a
/// Ctrl-C in the shell the dashboard was launched from, which is why it gets
/// its own process group.
pub fn spawn_daemon(paths: &Paths, port: u16) -> Result<Health, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Could not locate this executable to start a daemon: {error}"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("daemon")
        .arg("--port")
        .arg(port.to_string())
        .stdin(std::process::Stdio::null());
    // A daemon that dies on startup must not be invisible: this file is the
    // only place a detached process can say why, and `daemon status` and
    // `doctor` both read it back.
    let _ = std::fs::create_dir_all(&paths.runtime_dir);
    if let Ok(log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(super::protocol::daemon_log_path(paths))
    {
        if let Ok(errors) = log.try_clone() {
            command.stdout(log).stderr(errors);
        }
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .spawn()
        .map_err(|error| format!("Could not start the background daemon: {error}"))?;

    let deadline = Instant::now() + AUTOSTART_DEADLINE;
    while Instant::now() < deadline {
        if let Some(health) = probe(port, AUTOSTART_PROBE) {
            // Whatever answers is not necessarily what was just started: if
            // something else already had the port, ours died on `bind` and
            // this is the squatter waving back.
            if !health.is_this_implementation() {
                return Err(foreign_daemon(port, &health));
            }
            return Ok(health);
        }
        thread::sleep(AUTOSTART_POLL);
    }
    // It was started and it did not answer. Whatever it printed on the way
    // down is the answer, and it is in the log nobody would think to look at.
    let seconds = AUTOSTART_DEADLINE.as_secs();
    Err(match super::protocol::daemon_log_last_error(paths) {
        Some(line) => format!(
            "The daemon did not answer on :{port} within {seconds}s — daemon.log says: {line}"
        ),
        None => format!(
            "The daemon did not answer on :{port} within {seconds}s (see {}).",
            super::protocol::daemon_log_path(paths).display()
        ),
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

    /// The legacy notification contract `claude-sessions notify` posts.
    pub fn notify(&self, body: Value) -> Option<Response> {
        self.post("/notify", body)
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
