//! The tmux driver against a REAL tmux.
//!
//! The unit tests cover argv construction; these cover the parts only a live
//! tmux can prove: that a launched window really exists at the requested
//! directory, that a session the scanner found (which reports a bare `ttysNNN`)
//! can be addressed by tty, that the exported environment reaches the child, and
//! that closing kills the pane.
//!
//! Skipped — cleanly, with a printed reason — when tmux is not installed, so the
//! suite stays green on a machine without it.
//!
//! This is also the one place in the crate that asks for [`SpawnPolicy::Allow`].
//! It is allowed to spawn because spawning is the thing under test, and it is
//! bounded: every session it creates is named after this process and killed
//! again. The platform is pinned to [`Platform::Other`] for the same reason in
//! reverse — on macOS, `focus()` on an unattached session opens a real iTerm2
//! tab, and a test suite that leaves GUI tabs behind is exactly what the spawn
//! policy exists to prevent.

use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

use claude_sessions::term::{
    LaunchRequest, Platform, SessionRef, SpawnPolicy, TerminalDriver, TmuxDriver,
};

fn tmux(args: &[&str]) -> (bool, String) {
    match Command::new("tmux").args(args).output() {
        Ok(out) => (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        Err(_) => (false, String::new()),
    }
}

fn settle(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

/// A named session that is killed however the test ends.
struct Session {
    name: String,
    driver: TmuxDriver,
}

impl Session {
    fn new(tag: &str) -> Self {
        let name = format!("cs-test-{}-{tag}", std::process::id());
        let driver =
            TmuxDriver::new(name.clone(), SpawnPolicy::Allow).with_platform(Platform::Other);
        Session { name, driver }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        tmux(&["kill-session", "-t", &self.name]);
    }
}

/// `false` (having said so) when there is no tmux to test against.
fn available() -> bool {
    let present = TmuxDriver::new("probe", SpawnPolicy::Allow).is_available();
    if !present {
        eprintln!("skipping the live tmux suite: tmux is not installed");
    }
    present
}

/// Something to drive: a process that holds the pane open and nothing else.
///
/// `sh -i` (what the Node suite used) is flaky here — an interactive shell in a
/// window nobody is attached to sometimes gives up on its unread tty, and the
/// window vanishes between the launch and the assertion. A `sleep` has no job
/// control, no profile to source and no input to lose, so what it proves is the
/// driver rather than the shell.
fn keep_alive(cwd: &str, title: &str) -> LaunchRequest {
    LaunchRequest::new(cwd, "sleep 120").title(title)
}

#[test]
fn launch_creates_a_real_window_at_the_requested_cwd_and_says_how_to_attach() {
    if !available() {
        return;
    }
    let session = Session::new("launch");
    let cwd = std::env::current_dir().expect("cwd").display().to_string();

    let result = session.driver.launch(&keep_alive(&cwd, "under-test"));
    assert!(result.ok, "launch failed: {:?}", result.error);
    // A detached session nobody knows about is useless; the hint is the UX.
    assert!(
        result.hint.unwrap_or_default().contains("tmux attach -t"),
        "the first launch must say how to reach it"
    );
    settle(500);

    let (_, listing) = tmux(&[
        "list-windows",
        "-t",
        &session.name,
        "-F",
        "#{window_name}\t#{pane_current_path}",
    ]);
    let row = listing.lines().next().expect("a window");
    let (name, path) = row.split_once('\t').expect("name and path");
    assert_eq!(name, "under-test");
    assert_eq!(path, cwd);

    // A second launch adds a window rather than a second session, and does not
    // re-announce how to attach.
    let second = session.driver.launch(&keep_alive("/tmp", "second-window"));
    assert!(second.ok, "{:?}", second.error);
    assert_eq!(second.hint, None, "should not re-announce attaching");
    settle(300);
    let (_, names) = tmux(&["list-windows", "-t", &session.name, "-F", "#{window_name}"]);
    assert!(names.lines().any(|line| line == "second-window"), "{names}");
}

#[test]
fn addresses_a_session_by_the_bare_tty_the_scanner_reports() {
    if !available() {
        return;
    }
    let session = Session::new("tty");
    let cwd = std::env::current_dir().expect("cwd").display().to_string();
    assert!(session.driver.launch(&keep_alive(&cwd, "under-test")).ok);
    settle(500);

    let (_, pane_tty) = tmux(&[
        "list-panes",
        "-t",
        &format!("{}:under-test", session.name),
        "-F",
        "#{pane_tty}",
    ]);
    let pane_tty = pane_tty.lines().next().expect("a pane tty").to_string();

    // The scanner gets "ttys011" from ps; tmux reports "/dev/ttys011". This join
    // is what lets tmux act on sessions discovered the existing way.
    assert!(
        pane_tty.starts_with("/dev/"),
        "unexpected pane tty: {pane_tty}"
    );
    let bare = pane_tty.trim_start_matches("/dev/");
    let result = session.driver.focus(&SessionRef::from_tty(bare));
    assert!(
        result.ok,
        "the bare tty did not resolve to a pane: {:?}",
        result.error
    );
    // Nothing is attached, so focus reports how to see it instead of pretending.
    assert!(
        result
            .hint
            .unwrap_or_default()
            .contains("no attached client"),
        "focus must say how to look at an unattached session"
    );

    // And the failure cases say why rather than silently doing nothing.
    let unknown = session.driver.focus(&SessionRef::from_tty("ttys999"));
    assert!(!unknown.ok);
    assert!(unknown
        .error
        .unwrap_or_default()
        .contains("No tmux pane is attached"));

    for tty in ["??", "-", ""] {
        let result = session.driver.focus(&SessionRef::from_tty(tty));
        assert!(!result.ok, "{tty:?} was accepted");
        assert!(result.error.unwrap_or_default().contains("no TTY"));
    }
    let result = session.driver.focus(&SessionRef::default());
    assert!(!result.ok);
    assert!(result.error.unwrap_or_default().contains("no TTY"));
}

#[test]
fn the_launched_command_sees_the_exported_environment() {
    if !available() {
        return;
    }
    // `claude-sessions done` reads CLAUDE_SESSIONS_TASK_ID from its process
    // environment — if the export does not survive the launch, finished tasks
    // never report back. The command writes it out, since the dashboard can no
    // longer type into a pane to ask.
    let session = Session::new("env");
    let out = std::env::temp_dir().join(format!("cs-env-{}.txt", std::process::id()));
    let _ = fs::remove_file(&out);

    let request = LaunchRequest::new(
        std::env::current_dir().expect("cwd").display().to_string(),
        format!(
            "printf '%s' \"$CLAUDE_SESSIONS_TASK_ID\" > {}",
            out.display()
        ),
    )
    .task_id(5944)
    .title("env-probe");

    let result = session.driver.launch(&request);
    assert!(result.ok, "{:?}", result.error);
    settle(1200);

    assert_eq!(
        fs::read_to_string(&out).unwrap_or_default(),
        "5944",
        "the task id did not reach the child process"
    );
    let _ = fs::remove_file(&out);
}

#[test]
fn close_removes_the_pane_and_a_pane_already_gone_is_success() {
    if !available() {
        return;
    }
    // Killing the agent leaves the terminal at a shell prompt, so a purge that
    // only signalled processes freed a process but never a tab.
    let session = Session::new("close");
    assert!(session.driver.launch(&keep_alive("/tmp", "under-test")).ok);
    settle(500);

    let (_, row) = tmux(&[
        "list-panes",
        "-t",
        &format!("{}:under-test", session.name),
        "-F",
        "#{pane_tty}\t#{pane_id}",
    ]);
    let row = row.lines().next().expect("a pane");
    let (pane_tty, pane_id) = row.split_once('\t').expect("tty and id");
    let reference = SessionRef::from_tty(pane_tty);

    assert!(session.driver.close(&reference).ok);
    settle(300);
    // Checked by pane id, not by tty: a tty freed by one closing pane is handed
    // straight to the next window a sibling test opens, so a tty assertion here
    // fails at random under `--test-threads`. Pane ids are never reused.
    let (_, panes) = tmux(&["list-panes", "-a", "-F", "#{pane_id}"]);
    assert!(
        !panes.lines().any(|line| line == pane_id),
        "the pane survived close(): {panes}"
    );

    // Closing it again is the outcome the caller wanted, not a failure.
    assert!(session.driver.close(&reference).ok);
}
