//! Finding a daemon, and deciding whether it is one of ours.
//!
//! Split from the conversation itself because it answers a different question.
//! Everything next door assumes it is talking to this program; the job here is
//! to establish that it is — two programs have shipped under this name, they
//! bind the same port, and a dashboard attached to the wrong one connects,
//! subscribes, and shows nothing forever.

use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::paths::Paths;

use super::{request, AUTOSTART_DEADLINE, AUTOSTART_POLL, AUTOSTART_PROBE, PROBE_TIMEOUT};
use crate::daemon::protocol;

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
        self.implementation == protocol::IMPLEMENTATION
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
        "Port {port} is held by {} — its sessions cannot be read here. Stop it with \
         `claude-sessions daemon stop`, or give this one its own `daemon.port`.",
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
        .open(protocol::daemon_log_path(paths))
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
    Err(match protocol::daemon_log_last_error(paths) {
        Some(line) => format!(
            "The daemon did not answer on :{port} within {seconds}s — daemon.log says: {line}"
        ),
        None => format!(
            "The daemon did not answer on :{port} within {seconds}s (see {}).",
            protocol::daemon_log_path(paths).display()
        ),
    })
}
