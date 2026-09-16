//! iTerm2 terminal driver — macOS only, driven over AppleScript.
//!
//! Sessions are addressed by tty, which is how the scanner already identifies
//! them, so the AppleScript loops search windows/tabs/sessions for a matching
//! `tty of s`.
//!
//! Every script below is built by a pure function and run through one
//! `osascript` call, so the whole of the interesting behaviour is unit-tested
//! on a machine where iTerm2 will never open.

use std::time::Duration;

use super::exec::Exec;
use super::select::Platform;
use super::shell::{build_shell_command, chunk_text, SEND_CHUNK_SIZE};
use super::spawn::SpawnPolicy;
use super::types::{DriverResult, LaunchRequest, SessionRef, TerminalDriver};

const TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Pause between writes, for the reader to drain. See [`SEND_CHUNK_SIZE`].
const CHUNK_DELAY: &str = "0.05";
/// Pause before the Return. Writing the text and the return in one go can beat
/// the agent's input handler and lose the submission.
const SUBMIT_DELAY: &str = "0.5";

const NOT_FOUND: &str = "NOT_FOUND";

/// Escape for embedding inside an AppleScript double-quoted string.
pub fn escape_applescript(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape for embedding inside a single-quoted shell word.
pub fn escape_shell_single(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Open a new FOREGROUND tab running `shell_command`.
///
/// The sibling launch script deliberately returns focus to the previous tab,
/// because starting an agent should not steal your screen. This is the opposite
/// case: the tab exists so you can look at it, so leaving it in the background
/// would defeat the purpose.
///
/// It runs the command AS the session's program, rather than creating a tab and
/// typing into it. `write text` races the new tab's shell startup: characters
/// land while zsh is still printing its own prompt, so the line arrives
/// corrupted — observed as "etmux new-session …" (a stray leading character)
/// and as a whole command wrapped in an extra quote, which drops the shell into
/// a `quote>` continuation and hangs there.
///
/// The launch path lives with that race because a mangled agent command fails
/// loudly and gets relaunched. A viewer tab that hangs at `quote>` just looks
/// broken, so it gets the version with no race in it.
pub fn build_viewer_tab_script(shell_command: &str) -> String {
    let inner = format!("/bin/sh -lc '{}'", escape_shell_single(shell_command));
    format!(
        r#"
    tell application "iTerm2"
      activate
      tell current window
        create tab with default profile command "{command}"
      end tell
    end tell
  "#,
        command = escape_applescript(&inner)
    )
}

/// Open a new BACKGROUND tab running `shell_command`, keeping the current tab
/// focused — launching an agent must not take the screen away from whoever
/// launched it.
pub fn build_launch_script(shell_command: &str) -> String {
    format!(
        r#"
    tell application "iTerm2"
      tell current window
        set prevTab to current tab
        create tab with default profile
        tell current session
          write text "{command}"
        end tell
        select prevTab
      end tell
    end tell
  "#,
        command = escape_applescript(shell_command)
    )
}

/// Find the session on `tty_dev`, run `body` against it, and say which
/// happened: `OK`, or `NOT_FOUND` when no tab owns that tty.
pub(super) fn build_find_pane_script(tty_dev: &str, body: &str) -> String {
    format!(
        r#"
    set targetTTY to "{tty_dev}"
    tell application "iTerm2"
      repeat with w in windows
        repeat with t in tabs of w
          repeat with s in sessions of t
            if tty of s is targetTTY then
              {body}
              return "OK"
            end if
          end repeat
        end repeat
      end repeat
      return "NOT_FOUND"
    end tell
  "#
    )
}

/// Type text into the session on `tty_dev` and submit it.
///
/// One `write text` per chunk so no single write can overrun the terminal's
/// input queue, then a pause, then exactly one Return — last, and only once.
pub fn build_send_text_script(tty_dev: &str, text: &str) -> String {
    let writes = chunk_text(text, SEND_CHUNK_SIZE)
        .iter()
        .map(|chunk| {
            format!(
                "write text \"{}\" newline NO\n         delay {CHUNK_DELAY}",
                escape_applescript(chunk)
            )
        })
        .collect::<Vec<_>>()
        .join("\n         ");
    let body = format!(
        "tell s\n         {writes}\n         delay {SUBMIT_DELAY}\n         write text (ASCII character 13) newline NO\n       end tell"
    );
    build_find_pane_script(tty_dev, &body)
}

/// Bring the window, tab and split holding `tty_dev` to the front.
pub fn build_focus_script(tty_dev: &str) -> String {
    format!(
        r#"
    tell application "iTerm2"
      activate
      repeat with w in windows
        repeat with t in tabs of w
          repeat with s in sessions of t
            if tty of s is "{tty_dev}" then
              try
                tell w to select
              end try
              tell t to select
              tell s to select
              return "OK"
            end if
          end repeat
        end repeat
      end repeat
      return "NOT_FOUND"
    end tell
  "#
    )
}

/// Close the tab or split running on `tty_dev`.
pub fn build_close_script(tty_dev: &str) -> String {
    build_find_pane_script(tty_dev, "tell s to close")
}

/// Run a command in a new foreground iTerm2 tab. `false` when iTerm2 cannot be
/// driven — including when the spawn policy refuses.
pub(super) fn open_viewer_tab(exec: &Exec, shell_command: &str) -> bool {
    osascript(exec, &build_viewer_tab_script(shell_command), TIMEOUT).ok
}

fn osascript(exec: &Exec, script: &str, timeout: Duration) -> super::exec::CommandOutput {
    exec.run(
        "osascript",
        &["-e".to_string(), script.to_string()],
        timeout,
    )
}

// --- driver -----------------------------------------------------------------

pub struct Iterm2Driver {
    exec: Exec,
    platform: Platform,
}

impl Iterm2Driver {
    pub fn new(policy: SpawnPolicy) -> Self {
        Iterm2Driver {
            exec: Exec::new(policy),
            platform: Platform::current(),
        }
    }

    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    fn device(&self, session: &SessionRef, missing: &str) -> Result<String, DriverResult> {
        session
            .tty_device()
            .ok_or_else(|| DriverResult::failed(missing))
    }
}

impl TerminalDriver for Iterm2Driver {
    fn name(&self) -> &'static str {
        "iterm2"
    }

    fn is_available(&self) -> bool {
        if self.platform != Platform::MacOs {
            return false;
        }
        // The script's answer is irrelevant; what is being tested is whether
        // osascript runs at all, which is really a test of the machine's
        // automation permission.
        osascript(
            &self.exec,
            "tell application \"System Events\" to return (exists application process \"iTerm2\") or true",
            PROBE_TIMEOUT,
        )
        .ok
    }

    fn launch(&self, request: &LaunchRequest) -> DriverResult {
        let shell_command = build_shell_command(&request.cwd, &request.command, &request.env);
        let res = osascript(&self.exec, &build_launch_script(&shell_command), TIMEOUT);
        if res.ok {
            DriverResult::ok()
        } else {
            DriverResult::failed(res.failure_message())
        }
    }

    fn send_text(&self, session: &SessionRef, text: &str) -> DriverResult {
        let dev = match self.device(session, "This session has no TTY.") {
            Ok(dev) => dev,
            Err(result) => return result,
        };
        let res = osascript(&self.exec, &build_send_text_script(&dev, text), TIMEOUT);
        if !res.ok || res.stdout == NOT_FOUND {
            return DriverResult::failed(format!("Session TTY {dev} not found in iTerm2"));
        }
        DriverResult::ok()
    }

    fn focus(&self, session: &SessionRef) -> DriverResult {
        let dev = match self.device(session, "No terminal (TTY) for this session.") {
            Ok(dev) => dev,
            Err(result) => return result,
        };
        let res = osascript(&self.exec, &build_focus_script(&dev), TIMEOUT);
        if !res.ok || res.stdout == NOT_FOUND {
            return DriverResult::failed(format!("Couldn't find the iTerm2 tab for {dev}."));
        }
        DriverResult::ok()
    }

    fn close(&self, session: &SessionRef) -> DriverResult {
        let dev = match self.device(session, "No terminal (TTY) for this session.") {
            Ok(dev) => dev,
            Err(result) => return result,
        };
        let res = osascript(&self.exec, &build_close_script(&dev), TIMEOUT);
        // A tab that has already gone is the outcome we wanted, not a failure.
        if res.stdout == NOT_FOUND {
            return DriverResult::ok();
        }
        if !res.ok {
            return DriverResult::failed(format!("Couldn't close the iTerm2 tab for {dev}."));
        }
        DriverResult::ok()
    }
}
