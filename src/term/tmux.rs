//! tmux terminal driver.
//!
//! Why this works with the existing session scanner: every tmux pane owns a
//! pty, and `tmux list-panes` reports it. The scanner already records each
//! claude process's tty (from `ps`), so the tty is the join key between "a
//! session the dashboard found" and "a pane tmux can drive" — no scanner
//! changes needed.
//!
//! Unlike the iTerm2 driver this needs no GUI, no AppleScript, and no macOS: it
//! works over SSH and on Linux, and sessions survive the terminal being closed.
//!
//! Everything above [`TmuxDriver`] is a pure argv builder, so the command
//! construction is fully tested on a machine with no tmux installed.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use super::exec::Exec;
use super::iterm2;
use super::select::Platform;
use super::shell::{build_shell_command, chunk_text, shell_quote, SEND_CHUNK_SIZE};
use super::spawn::SpawnPolicy;
use super::types::{normalize_tty, DriverResult, LaunchRequest, SessionRef, TerminalDriver};

const TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// Let the pane's line discipline drain between writes.
const CHUNK_PAUSE: Duration = Duration::from_millis(50);

/// A detached session gets tmux's default 80x24, which is narrow enough to
/// matter: it is the width Claude Code renders its TUI at, and the width a
/// copied line wraps to. QA reports and diffs are unreadable in 80 columns.
pub const DETACHED_WIDTH: u16 = 200;
pub const DETACHED_HEIGHT: u16 = 50;

/// `list-panes` format: the tty first, because it is what everything joins on.
pub const LIST_PANES_FORMAT: &str = "#{pane_tty}\t#{pane_id}\t#{session_name}\t#{window_index}";

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

pub fn list_panes_args() -> Vec<String> {
    args(&["list-panes", "-a", "-F", LIST_PANES_FORMAT])
}

pub fn build_new_session_args(
    session_name: &str,
    cwd: &str,
    shell_command: &str,
    title: Option<&str>,
) -> Vec<String> {
    let width = DETACHED_WIDTH.to_string();
    let height = DETACHED_HEIGHT.to_string();
    let mut out = args(&[
        "new-session",
        "-d",
        "-s",
        session_name,
        "-c",
        cwd,
        "-x",
        &width,
        "-y",
        &height,
    ]);
    if let Some(title) = title {
        out.extend(args(&["-n", title]));
    }
    out.extend(args(&["/bin/sh", "-lc", shell_command]));
    out
}

pub fn build_new_window_args(
    session_name: &str,
    cwd: &str,
    shell_command: &str,
    title: Option<&str>,
) -> Vec<String> {
    // `-d`: create it in the background so the dashboard keeps focus, matching
    // the iTerm2 driver's "select prevTab" behaviour.
    let target = format!("{session_name}:");
    let mut out = args(&["new-window", "-d", "-t", &target, "-c", cwd]);
    if let Some(title) = title {
        out.extend(args(&["-n", title]));
    }
    out.extend(args(&["/bin/sh", "-lc", shell_command]));
    out
}

/// `-l` sends the text literally, so a prompt containing `;`, `Enter` or any
/// other tmux key name is delivered as characters rather than interpreted.
pub fn build_send_text_args(pane_id: &str, text: &str) -> Vec<String> {
    args(&["send-keys", "-t", pane_id, "-l", text])
}

/// One `send-keys` invocation per chunk, in order. See
/// [`SEND_CHUNK_SIZE`](super::shell::SEND_CHUNK_SIZE) for why it is split at
/// all: `send-keys` writes into the pane's pty, which holds about 1KB before
/// the line discipline starts discarding.
pub fn build_send_text_args_chunked(pane_id: &str, text: &str) -> Vec<Vec<String>> {
    chunk_text(text, SEND_CHUNK_SIZE)
        .iter()
        .map(|chunk| build_send_text_args(pane_id, chunk))
        .collect()
}

/// A SEPARATE call from the text, deliberately: `Enter` is a tmux key name and
/// can only be sent without `-l`.
pub fn build_send_enter_args(pane_id: &str) -> Vec<String> {
    args(&["send-keys", "-t", pane_id, "Enter"])
}

pub fn build_kill_pane_args(pane_id: &str) -> Vec<String> {
    args(&["kill-pane", "-t", pane_id])
}

/// Name of the throwaway session a viewer tab attaches to.
///
/// Attaching two tabs to the SAME tmux session makes them share window
/// selection — open task A in one tab and task B in another, and both jump to
/// whichever was selected last. A grouped session shares the windows but keeps
/// its own selection, so the name is per-window rather than per-session.
pub fn build_viewer_session_name(target: &str, window_index: &str) -> String {
    format!("{target}-view{window_index}")
}

/// Shell line that opens a viewer onto one window.
///
/// Selection happens BEFORE the attach, deliberately: doing it afterwards needs
/// `tmux attach \; select-window`, and that backslash has to survive both
/// AppleScript and shell quoting on the way to the terminal. Three plain
/// commands have nothing to escape.
///
/// There is deliberately NO `destroy-unattached` here, though it looks like the
/// obvious way to clean the viewer up. The session is created detached, so
/// setting that option destroys it on the spot — before anything can attach —
/// and the attach then fails with `can't find session: <viewer>`. The tidy-up
/// would cost the feature.
///
/// Leaving viewers behind is cheap instead of leaky: `new-session -d -t` fails
/// harmlessly when one already exists, so re-opening the same task reuses its
/// viewer rather than stacking another. The count is bounded by how many
/// windows the real session has, and a viewer holds no processes of its own.
pub fn build_attach_shell_command(target: &str, window_index: &str) -> String {
    let viewer = build_viewer_session_name(target, window_index);
    let select = format!("{viewer}:{window_index}");
    [
        format!(
            "tmux new-session -d -t {} -s {} 2>/dev/null",
            shell_quote(target),
            shell_quote(&viewer)
        ),
        format!("tmux select-window -t {} 2>/dev/null", shell_quote(&select)),
        format!("tmux attach -t {}", shell_quote(&viewer)),
    ]
    .join("; ")
}

/// One row of `list-panes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String,
    pub session_name: String,
    pub window_index: String,
}

/// Parse `list-panes` output into a tty -> pane index.
pub fn parse_pane_list(stdout: &str) -> HashMap<String, Pane> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(tty), Some(pane_id)) = (fields.next(), fields.next()) else {
            continue;
        };
        if tty.is_empty() || pane_id.is_empty() {
            continue;
        }
        map.insert(
            tty.to_string(),
            Pane {
                pane_id: pane_id.to_string(),
                session_name: fields.next().unwrap_or_default().to_string(),
                window_index: fields.next().unwrap_or_default().to_string(),
            },
        );
    }
    map
}

// --- driver -----------------------------------------------------------------

pub struct TmuxDriver {
    target: String,
    exec: Exec,
    platform: Platform,
}

impl TmuxDriver {
    pub fn new(session_name: impl Into<String>, policy: SpawnPolicy) -> Self {
        TmuxDriver {
            target: session_name.into(),
            exec: Exec::new(policy),
            platform: Platform::current(),
        }
    }

    /// Pin the platform, so the viewer-tab branch is reachable from a test on
    /// any machine.
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    pub fn session_name(&self) -> &str {
        &self.target
    }

    fn find_pane(&self, session: &SessionRef) -> Result<Pane, String> {
        let Some(dev) = normalize_tty(session.tty.as_deref()) else {
            return Err("This session has no TTY, so tmux cannot address it.".to_string());
        };
        let res = self.exec.run("tmux", &list_panes_args(), TIMEOUT);
        if !res.ok {
            return Err(format!("tmux list-panes failed: {}", res.failure_message()));
        }
        parse_pane_list(&res.stdout)
            .remove(&dev)
            .ok_or_else(|| format!("No tmux pane is attached to {dev}."))
    }

    fn session_exists(&self) -> bool {
        self.exec
            .run("tmux", &args(&["has-session", "-t", &self.target]), TIMEOUT)
            .ok
    }
}

impl TerminalDriver for TmuxDriver {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn is_available(&self) -> bool {
        self.exec.run("tmux", &args(&["-V"]), VERSION_TIMEOUT).ok
    }

    fn launch(&self, request: &LaunchRequest) -> DriverResult {
        let shell_command = build_shell_command(&request.cwd, &request.command, &request.env);
        let exists = self.session_exists();
        let argv = if exists {
            build_new_window_args(
                &self.target,
                &request.cwd,
                &shell_command,
                request.title.as_deref(),
            )
        } else {
            build_new_session_args(
                &self.target,
                &request.cwd,
                &shell_command,
                request.title.as_deref(),
            )
        };

        let res = self.exec.run("tmux", &argv, TIMEOUT);
        if !res.ok {
            return DriverResult::failed(res.failure_message());
        }
        if exists {
            DriverResult::ok()
        } else {
            // A detached session nobody knows about is useless; the hint is the
            // whole UX of the first launch.
            DriverResult::ok_with_hint(format!(
                "Started tmux session \"{0}\" — attach with: tmux attach -t {0}",
                self.target
            ))
        }
    }

    fn send_text(&self, session: &SessionRef, text: &str) -> DriverResult {
        let pane = match self.find_pane(session) {
            Ok(pane) => pane,
            Err(error) => return DriverResult::failed(error),
        };
        for argv in build_send_text_args_chunked(&pane.pane_id, text) {
            let typed = self.exec.run("tmux", &argv, TIMEOUT);
            if !typed.ok {
                return DriverResult::failed(typed.failure_message());
            }
            thread::sleep(CHUNK_PAUSE);
        }
        let submitted = self
            .exec
            .run("tmux", &build_send_enter_args(&pane.pane_id), TIMEOUT);
        if !submitted.ok {
            return DriverResult::failed(submitted.failure_message());
        }
        DriverResult::ok()
    }

    fn focus(&self, session: &SessionRef) -> DriverResult {
        let pane = match self.find_pane(session) {
            Ok(pane) => pane,
            Err(error) => return DriverResult::failed(error),
        };
        let window = format!("{}:{}", pane.session_name, pane.window_index);
        let selected = self
            .exec
            .run("tmux", &args(&["select-window", "-t", &window]), TIMEOUT);
        if !selected.ok {
            return DriverResult::failed(selected.failure_message());
        }
        let _ = self.exec.run(
            "tmux",
            &args(&["select-pane", "-t", &pane.pane_id]),
            TIMEOUT,
        );

        // Selecting only moves tmux's own cursor. With nobody attached that is
        // invisible, and telling the user to go and type `tmux attach`
        // themselves is not "open the session" — it is homework.
        let attached = self.exec.run(
            "tmux",
            &args(&["list-clients", "-t", &pane.session_name]),
            TIMEOUT,
        );
        if attached.ok && !attached.stdout.is_empty() {
            return DriverResult::ok();
        }

        let attach = build_attach_shell_command(&pane.session_name, &pane.window_index);

        // So open a viewer tab instead. This is the hybrid worth having: a real
        // tab you can look at, over a session that keeps running when you close
        // it. An iTerm2-native session cannot offer the second half.
        //
        // No guard of its own: opening a tab goes through the same Exec, and so
        // through the same SpawnPolicy, as every other spawn here — a test that
        // reaches focus() gets a refusal and the hint below, not a stray tab.
        if self.platform == Platform::MacOs && iterm2::open_viewer_tab(&self.exec, &attach) {
            return DriverResult::ok_with_hint(format!(
                "Opened a tab attached to {window}. Closing it leaves the session running."
            ));
        }

        // No GUI to open a tab in — over SSH, or on Linux. The instruction is
        // still the right answer there, so fall back to it rather than failing.
        DriverResult::ok_with_hint(format!(
            "tmux session \"{}\" has no attached client — run: {attach}",
            pane.session_name
        ))
    }

    fn close(&self, session: &SessionRef) -> DriverResult {
        // No pane means nothing left to close, which is where we wanted to end
        // up.
        let Ok(pane) = self.find_pane(session) else {
            return DriverResult::ok();
        };
        let killed = self
            .exec
            .run("tmux", &build_kill_pane_args(&pane.pane_id), TIMEOUT);
        if !killed.ok {
            return DriverResult::failed(killed.failure_message());
        }
        DriverResult::ok()
    }
}
