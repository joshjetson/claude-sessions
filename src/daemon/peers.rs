//! Auto QA across your own machines: a project is on in one place only.
//!
//! Auto QA is switched on per machine, in each machine's config. With it on for
//! one project on two machines, both would start the same task, and the second
//! pass would cost a session and a full QA run for nothing. Odoo cannot hold
//! the answer: QA writes nothing to Odoo. So the daemons ask each other,
//! directly, over Tailscale.
//!
//! Each daemon with a peer secret runs a second, tiny listener on its
//! Tailscale address. It answers one question, and only with the secret:
//! which projects have Auto QA on here. It is a server of its own, apart from
//! the daemon's main one, so none of the main routes can ever be reached from
//! another machine.
//!
//! Before each Auto QA pass, a daemon asks its peers. A project on in two
//! places stays on at the machine whose address sorts first and is paused at
//! the other. Both machines apply the same rule to the same two answers, so
//! they agree without talking again. The pause is not written to the config:
//! it lasts while the conflict does, and the notification asks the person to
//! switch the project off at one machine.
//!
//! A peer that does not answer is asleep, offline or not set up, and this
//! machine carries on. The check protects whenever both machines are up.

use std::io::{self, BufReader};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::protocol::{read_body, read_head};

/// The listener's port unless the config names another.
pub const DEFAULT_PEER_PORT: u16 = 8788;
/// The one path the listener answers.
pub const PEER_PATH: &str = "/peer/auto-qa";
/// A secret shorter than this is refused: it would be guessable.
pub const MIN_SECRET_LEN: usize = 16;
/// How long a peer has to answer. A peer that is asleep must not hold up the
/// slow tick for long.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(2);
/// The request body is one secret, so anything bigger is not a peer.
const MAX_BODY: usize = 4096;
/// Connections handled at once. A peer asks once a tick.
const MAX_CONNECTIONS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// What a peer says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PeerAnswer {
    /// The address its listener is bound to. What the tie-break sorts on.
    pub id: String,
    /// The projects with Auto QA on there.
    pub projects: Vec<String>,
}

/// A project to pause here, and the peer that keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paused {
    pub project: String,
    pub peer: String,
}

/// Which of this machine's Auto QA projects to pause, given what the peers
/// said. `peers` pairs how each peer is named in the config with its answer.
///
/// A project on at a peer is paused here when the peer's id sorts before this
/// machine's. With no id of its own, because its listener is not running, this
/// machine cannot take part in the tie-break, and it pauses: two machines that
/// both keep a project is the one outcome the check exists to stop.
pub fn paused_projects(
    my_id: Option<&str>,
    mine: &[String],
    peers: &[(String, PeerAnswer)],
) -> Vec<Paused> {
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let mut out = Vec::new();
    for project in mine {
        let keeper = peers.iter().find(|(_, answer)| {
            answer.projects.iter().any(|theirs| same(theirs, project))
                && my_id.is_none_or(|me| answer.id.as_str() < me)
        });
        if let Some((peer, _)) = keeper {
            out.push(Paused {
                project: project.clone(),
                peer: peer.clone(),
            });
        }
    }
    out
}

/// Compare two secrets in time that does not depend on where they differ.
pub fn secrets_match(given: &str, expected: &str) -> bool {
    let (given, expected) = (given.as_bytes(), expected.as_bytes());
    if given.len() != expected.len() {
        return false;
    }
    given
        .iter()
        .zip(expected)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// Ask one peer which projects have Auto QA on there. `None` when it does not
/// answer, refuses the secret, or answers with something else.
pub fn ask_peer(host: &str, port: u16, secret: &str) -> Option<PeerAnswer> {
    let body = json!({ "secret": secret }).to_string();
    let response =
        super::client::request_to(host, port, "POST", PEER_PATH, Some(&body), PEER_TIMEOUT)?;
    if response.status != 200 {
        return None;
    }
    serde_json::from_value(response.body).ok()
}

/// The running listener.
pub struct PeerListener {
    port: u16,
    stop: Arc<AtomicBool>,
    address: String,
}

impl PeerListener {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stop accepting. A connection in flight finishes.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &format!("{}:{}", self.address, self.port)
                .parse()
                .unwrap_or_else(|_| ([127, 0, 0, 1], self.port).into()),
            Duration::from_millis(200),
        );
    }
}

/// Answer peers on `listener`. `answer` says what to report, read fresh for
/// each request so an Auto QA switch shows at once.
pub fn serve_peers(
    listener: TcpListener,
    secret: String,
    answer: impl Fn() -> PeerAnswer + Send + Sync + 'static,
) -> io::Result<PeerListener> {
    let local = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new((secret, answer, AtomicUsize::new(0)));
    let accept_stop = Arc::clone(&stop);
    thread::Builder::new()
        .name("claude-sessions-peers".into())
        .spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                if accept_stop.load(Ordering::SeqCst) {
                    break;
                }
                let shared = Arc::clone(&shared);
                if shared.2.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                shared.2.fetch_add(1, Ordering::Relaxed);
                let spawned = thread::Builder::new()
                    .name("claude-sessions-peer".into())
                    .spawn(move || {
                        let (secret, answer, open) = &*shared;
                        handle(stream, secret, answer);
                        open.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    break;
                }
            }
        })?;
    Ok(PeerListener {
        port: local.port(),
        stop,
        address: local.ip().to_string(),
    })
}

fn handle(stream: TcpStream, secret: &str, answer: &dyn Fn() -> PeerAnswer) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let Ok(Some(head)) = read_head(&mut reader) else {
        return;
    };
    let (method, target, _) = head.parts();
    let path = target.split('?').next().unwrap_or("/");
    let (status, payload) = if method != "POST" || path != PEER_PATH {
        (404, json!({ "ok": false }))
    } else {
        let given = read_body(&mut reader, head.content_length(), MAX_BODY)
            .ok()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
            .and_then(|body| {
                body.get("secret")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if secrets_match(&given, secret) {
            (
                200,
                serde_json::to_value(answer()).unwrap_or_else(|_| json!({})),
            )
        } else {
            (403, json!({ "ok": false }))
        }
    };
    let _ = write_json(&stream, status, &payload);
    let _ = stream.shutdown(Shutdown::Both);
}

fn write_json(mut out: &TcpStream, status: u16, payload: &serde_json::Value) -> io::Result<()> {
    use std::io::Write;
    let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    out.write_all(head.as_bytes())?;
    out.write_all(&body)?;
    out.flush()
}
