//! The wire contract: how a client finds the daemon, and the framing both
//! halves agree on.
//!
//! Kept between [`server`](super::server) and [`client`](super::client) so
//! there is exactly one definition of the discovery file, the port rules and
//! the SSE frame — the Node app had `resolvePort` in `protocol.ts` and a
//! hand-rolled copy of it in `bin/notify.js`, which is how the helper CLI and
//! the daemon could disagree about which port to use.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::config::ConfigHandle;
use crate::paths::Paths;

use super::events::EngineEvent;

/// The port everything falls back to when nothing else names one. Defined once
/// in [`crate::config`] and re-exported here because this is the module both
/// sides of the wire import.
pub use crate::config::DEFAULT_PORT;

// --- who is on the port -----------------------------------------------------

/// What this program calls itself in `/health`.
///
/// Two different programs have shipped under this name — the original Node
/// tool and this one — and they bind the same port by default. With no marker
/// each happily mirrors the other's daemon: the dashboard connects, subscribes,
/// and receives a stream whose events it does not understand, so it shows an
/// empty list forever with nothing wrong anywhere. Three developers lost days
/// to exactly that. A daemon now says which program it is, and a dashboard
/// that does not recognise the answer scans for itself and says why.
pub const IMPLEMENTATION: &str = "claude-sessions-rs";

/// This build's version, as `/health` reports it. Not used to accept or refuse
/// a daemon — the implementation marker does that — but it is the first thing
/// anybody needs when two versions are involved.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `~/.claude-sessions/notify.json` — how `claude-sessions notify` and a
/// starting dashboard find a running daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    pub port: u16,
    pub pid: u32,
    /// True for a real daemon; the legacy in-TUI server omitted it.
    #[serde(default)]
    pub daemon: bool,
    #[serde(default)]
    pub started_at: String,
}

/// Read the discovery file. Anything unreadable or unparseable is "no daemon"
/// — a corrupt file must never stop the dashboard starting.
pub fn read_daemon_info(paths: &Paths) -> Option<DaemonInfo> {
    serde_json::from_str(&fs::read_to_string(&paths.port_file).ok()?).ok()
}

/// Announce this process as the daemon on `port`.
pub fn write_daemon_info(paths: &Paths, port: u16) -> io::Result<DaemonInfo> {
    let info = DaemonInfo {
        port,
        pid: std::process::id(),
        daemon: true,
        started_at: crate::util::iso_now(),
    };
    fs::create_dir_all(&paths.runtime_dir)?;
    let mut file = fs::File::create(&paths.port_file)?;
    file.write_all(serde_json::to_string(&info).unwrap_or_default().as_bytes())?;
    Ok(info)
}

/// Remove the discovery file on the way out, but only if it still points at
/// `pid` — a newer daemon may have taken the port over, and deleting its file
/// would leave every helper CLI falling back to the default port.
pub fn remove_daemon_info(paths: &Paths, pid: u32) -> bool {
    match read_daemon_info(paths) {
        Some(info) if info.pid == pid => fs::remove_file(&paths.port_file).is_ok(),
        _ => false,
    }
}

// --- the daemon's own output ------------------------------------------------

/// Where a daemon started by the dashboard sends its stdout and stderr.
///
/// The one place a detached daemon can say anything: it has no terminal, and
/// its refusals ("port already in use", a panic on startup) are exactly the
/// sentences somebody staring at an empty dashboard needs. Named here rather
/// than at the two places that open it, so they cannot drift apart.
pub fn daemon_log_path(paths: &Paths) -> std::path::PathBuf {
    paths.runtime_dir.join("daemon.log")
}

/// How much of the end of the log is ever read. A daemon appends for weeks;
/// nothing older than the last few lines is worth a syscall.
const LOG_TAIL_BYTES: u64 = 8 * 1024;

/// The last `lines` non-empty lines of the daemon log, oldest first.
pub fn daemon_log_tail(paths: &Paths, lines: usize) -> Vec<String> {
    let path = daemon_log_path(paths);
    let Ok(mut file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let from = size.saturating_sub(LOG_TAIL_BYTES);
    if io::Seek::seek(&mut file, io::SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    if io::Read::read_to_end(&mut file, &mut raw).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&raw);
    let mut tail: Vec<&str> = text.lines().collect();
    // The first line of a seeked read is half a line.
    if from > 0 && !tail.is_empty() {
        tail.remove(0);
    }
    tail.into_iter()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(lines)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Words that mark a log line as the reason something did not work.
const FAILURE_WORDS: [&str; 7] = [
    "error",
    "panic",
    "refus",
    "already in use",
    "could not",
    "cannot",
    "failed",
];

/// The most recent line of the daemon log that reads like a failure.
///
/// Deliberately a keyword sweep rather than a log format: the daemon's output
/// is whatever it printed plus whatever the runtime printed for it, and a
/// panic message obeys nobody's schema.
pub fn daemon_log_last_error(paths: &Paths) -> Option<String> {
    daemon_log_tail(paths, 40).into_iter().rev().find(|line| {
        let lower = line.to_ascii_lowercase();
        FAILURE_WORDS.iter().any(|word| lower.contains(word))
    })
}

/// Which port to talk to.
///
/// Precedence, unchanged from the Node original: an explicit `--port`, then
/// `CLAUDE_SESSIONS_NOTIFY_PORT`, then `daemon.port` (or the legacy
/// `notifyPort`), then whatever a running daemon advertises in `notify.json`,
/// then [`DEFAULT_PORT`]. The first four are [`ConfigHandle::resolve_port`]'s
/// job; this adds the discovery file, which is the only part that touches disk.
pub fn resolve_port(config: &ConfigHandle, paths: &Paths, explicit: Option<u16>) -> u16 {
    config.resolve_port(explicit, read_daemon_info(paths).map(|info| info.port))
}

// --- SSE framing ------------------------------------------------------------

/// The keep-alive. A comment frame: a client skips it, a dead socket fails the
/// write, which is how a vanished client is noticed between events.
pub const SSE_PING: &str = ": ping\n\n";

/// One server-sent event. `data` is already-serialised JSON.
pub fn sse_frame(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// An engine event as the frame that carries it.
///
/// The wire format *is* [`EngineEvent`]'s serde: the adjacent tagging names the
/// event and boxes the payload, so the SSE event name and its `data` come out
/// of one derive rather than a hand-written mapping table (the Node server kept
/// a twelve-row `wire` array that had to be edited in lockstep with the engine).
pub fn event_frame(event: &EngineEvent) -> Option<String> {
    let mut value = serde_json::to_value(event).ok()?;
    let data = value
        .get_mut("data")
        .map(serde_json::Value::take)
        .unwrap_or(serde_json::Value::Null);
    Some(sse_frame(event.name(), &data.to_string()))
}

// --- the little bit of HTTP both sides need ---------------------------------

/// Largest head (start line plus headers) either side will read. A client that
/// sends more than this is not one of ours.
const MAX_HEAD: usize = 16 * 1024;

/// The start line of an HTTP message plus its headers, lower-cased by name.
#[derive(Debug, Default)]
pub(crate) struct Head {
    pub(crate) start: String,
    pub(crate) headers: HashMap<String, String>,
}

impl Head {
    /// `Content-Length`, or zero when absent — a body-less request and one that
    /// forgot to say are handled the same way.
    pub(crate) fn content_length(&self) -> usize {
        self.headers
            .get("content-length")
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The three space-separated fields of the start line.
    pub(crate) fn parts(&self) -> (&str, &str, &str) {
        let mut fields = self.start.split_whitespace();
        (
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
            fields.next().unwrap_or_default(),
        )
    }
}

/// Read up to the blank line. `None` means the peer closed before sending one.
pub(crate) fn read_head(reader: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut head = Head::default();
    let mut read = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        read += line.len();
        if read > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too large"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return Ok(Some(head).filter(|head| !head.start.is_empty()));
        }
        if head.start.is_empty() {
            head.start = trimmed.to_string();
        } else if let Some((name, value)) = trimmed.split_once(':') {
            head.headers
                .insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
}

/// Read exactly `len` bytes of body, refusing anything over `max`.
pub(crate) fn read_body(reader: &mut impl BufRead, len: usize, max: usize) -> io::Result<Vec<u8>> {
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// `%20` and friends, for the project name in `/deploy/log/:project`.
pub(crate) fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not an escape after all — a literal percent sign.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
