//! The contract every terminal driver implements.
//!
//! The dashboard talks only to [`TerminalDriver`], so the macOS/iTerm2
//! dependency is one implementation rather than a property of the whole tool.

use crate::types::Session;

/// Environment variable a launched agent reads to know which task it is on.
/// Named once here; `bin/done` and the scanner both look for this exact key.
pub const TASK_ID_ENV: &str = "CLAUDE_SESSIONS_TASK_ID";

/// Everything a driver needs to open a new session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchRequest {
    /// Directory the new session starts in.
    pub cwd: String,
    /// Shell command line to run (already fully composed).
    pub command: String,
    /// Extra environment exported before the command runs. A `Vec` rather than
    /// a map because the order is part of the line the tests pin.
    pub env: Vec<(String, String)>,
    /// Human label, where the driver supports naming a window or tab.
    pub title: Option<String>,
}

impl LaunchRequest {
    pub fn new(cwd: impl Into<String>, command: impl Into<String>) -> Self {
        LaunchRequest {
            cwd: cwd.into(),
            command: command.into(),
            env: Vec::new(),
            title: None,
        }
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The one environment variable every pipeline launch sets.
    pub fn task_id(self, task_id: i64) -> Self {
        self.env(TASK_ID_ENV, task_id.to_string())
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Enough of a live session to address its terminal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRef {
    /// Short tty name as `ps` reports it, e.g. `ttys004`. The join key.
    pub tty: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

impl SessionRef {
    pub fn from_tty(tty: impl Into<String>) -> Self {
        SessionRef {
            tty: Some(tty.into()),
            ..SessionRef::default()
        }
    }

    pub fn from_session(session: &Session) -> Self {
        SessionRef {
            tty: session.tty.clone(),
            session_id: Some(session.session_id.clone()),
            cwd: Some(session.cwd.clone()),
        }
    }

    /// This session's tty as a device path, or `None` when it has no terminal
    /// for a driver to address. See [`normalize_tty`].
    pub fn tty_device(&self) -> Option<String> {
        normalize_tty(self.tty.as_deref())
    }
}

/// `ps` reports `ttys004`; tmux and iTerm2 both report `/dev/ttys004`.
///
/// This single mapping is the join between "a session the scanner found" and "a
/// pane a driver can drive" — it is why the terminal layer needed no scanner
/// changes at all. `??` and `-` are how `ps` spells "no controlling terminal".
/// (The Node app wrote this twice, once per driver.)
pub fn normalize_tty(tty: Option<&str>) -> Option<String> {
    let tty = tty?;
    if tty.is_empty() || tty == "??" || tty == "-" {
        return None;
    }
    if tty.starts_with("/dev/") {
        Some(tty.to_string())
    } else {
        Some(format!("/dev/{tty}"))
    }
}

/// The verdict of a driver call. Never an error type: a terminal that cannot be
/// driven is something the dashboard reports in its status line, not something
/// that unwinds a refresh.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DriverResult {
    pub ok: bool,
    pub error: Option<String>,
    /// Human-readable follow-up, e.g. how to attach to a tmux session.
    pub hint: Option<String>,
}

impl DriverResult {
    pub fn ok() -> Self {
        DriverResult {
            ok: true,
            ..DriverResult::default()
        }
    }

    pub fn ok_with_hint(hint: impl Into<String>) -> Self {
        DriverResult {
            ok: true,
            hint: Some(hint.into()),
            ..DriverResult::default()
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        DriverResult {
            ok: false,
            error: Some(error.into()),
            ..DriverResult::default()
        }
    }
}

pub trait TerminalDriver: Send + Sync {
    /// `tmux`, `iterm2`, or `none` for the null driver.
    fn name(&self) -> &'static str;

    /// Can this driver actually drive a terminal on this machine right now?
    fn is_available(&self) -> bool;

    fn launch(&self, request: &LaunchRequest) -> DriverResult;

    /// Type text into a live session and submit it.
    ///
    /// Deliberately narrow: this exists so a revision can be handed to a session
    /// that is already open, instead of starting a second process against the
    /// same conversation. It is not a general "drive the agent from the
    /// dashboard" facility — that surface was removed on purpose.
    fn send_text(&self, session: &SessionRef, text: &str) -> DriverResult;

    /// Bring the session's terminal to the foreground.
    fn focus(&self, session: &SessionRef) -> DriverResult;

    /// Close the session's tab or pane.
    ///
    /// Killing the agent leaves the terminal sitting at a shell prompt, so a
    /// purge that only signalled processes freed no space on the tab bar. A tab
    /// that has already gone counts as success — the caller wanted it absent,
    /// not removed by this particular call.
    fn close(&self, session: &SessionRef) -> DriverResult;
}

/// Reports a clear reason instead of failing silently, for when neither
/// terminal can be driven.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullDriver;

const NO_DRIVER: &str = "No terminal driver available.";
const NO_DRIVER_LAUNCH: &str =
    "No terminal driver available. Install tmux, or run under iTerm2 on macOS.";

impl TerminalDriver for NullDriver {
    fn name(&self) -> &'static str {
        "none"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn launch(&self, _request: &LaunchRequest) -> DriverResult {
        DriverResult::failed(NO_DRIVER_LAUNCH)
    }

    fn send_text(&self, _session: &SessionRef, _text: &str) -> DriverResult {
        DriverResult::failed(NO_DRIVER)
    }

    fn focus(&self, _session: &SessionRef) -> DriverResult {
        DriverResult::failed(NO_DRIVER)
    }

    fn close(&self, _session: &SessionRef) -> DriverResult {
        DriverResult::failed(NO_DRIVER)
    }
}
