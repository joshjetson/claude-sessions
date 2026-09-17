//! The thinnest possible shell-out layer.
//!
//! Everything above this file builds argv vectors and AppleScript strings and
//! is therefore unit-testable with no process anywhere near it. This is the
//! only place in [`crate::term`] that actually starts one, and it starts
//! nothing without [`SpawnPolicy::check`] agreeing first.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::spawn::SpawnPolicy;

/// What a driver learns from running a command. Mirrors the Node wrapper's
/// resolved object: never a rejection, always a verdict — a terminal that
/// cannot be driven is an outcome the UI reports, not an exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    /// Set when the command could not be run or did not finish: a missing
    /// binary, a non-zero exit, a timeout, a refusal.
    pub error: Option<String>,
}

impl CommandOutput {
    fn failed(error: String) -> Self {
        CommandOutput {
            ok: false,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(error),
        }
    }

    /// The sentence to show for a failure: whatever the command said on stderr,
    /// else why it never ran. Both drivers report failures the same way.
    pub fn failure_message(&self) -> String {
        if !self.stderr.is_empty() {
            return self.stderr.clone();
        }
        self.error
            .clone()
            .unwrap_or_else(|| "the command failed".to_string())
    }
}

/// The platform's "hand this to whatever handles it" command. The Node app was
/// macOS-only and said `open`; the rest of the world says `xdg-open`.
pub const OPEN_COMMAND: &str = if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};

/// How long `open` may take. It hands off to another process and returns, so
/// anything slower than this is a wedged desktop, not a slow launch.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Runs external commands on behalf of a driver.
#[derive(Debug, Clone, Copy)]
pub struct Exec {
    policy: SpawnPolicy,
}

impl Exec {
    pub fn new(policy: SpawnPolicy) -> Self {
        Exec { policy }
    }

    pub fn policy(&self) -> SpawnPolicy {
        self.policy
    }

    /// Run `program` with `args`, capturing both streams.
    ///
    /// The wait happens on a helper thread so a hung terminal cannot stall the
    /// dashboard, and the pipes are drained on that same thread: a child whose
    /// output fills the pipe buffer blocks until somebody reads it.
    pub fn run(&self, program: &str, args: &[String], timeout: Duration) -> CommandOutput {
        self.run_in(program, args, None, timeout)
    }

    /// Open a file or URL with the desktop's default handler.
    ///
    /// One implementation for the four callers that need it — the browser
    /// action, the daily log, and the two generated HTML viewers — so the
    /// platform choice and the spawn gate are decided once.
    pub fn open(&self, target: &str) -> CommandOutput {
        self.run(
            OPEN_COMMAND,
            std::slice::from_ref(&target.to_string()),
            OPEN_TIMEOUT,
        )
    }

    /// [`Exec::run`] with an explicit working directory.
    ///
    /// A parameter rather than a second near-identical function (WORKING.md
    /// rule 5). Two callers need it: the usage check runs in `$HOME` so a
    /// repository's `CLAUDE.md` cannot change what it reports, and the QAden
    /// staleness probe runs `git -C` against a worktree.
    pub fn run_in(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout: Duration,
    ) -> CommandOutput {
        if let Err(refused) = self.policy.check(&format!("run {program}")) {
            return CommandOutput::failed(refused.message);
        }

        let mut command = Command::new(program);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(err) => return CommandOutput::failed(format!("could not run {program}: {err}")),
        };

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let status = child.wait();
            let _ = tx.send((status, stdout, stderr));
        });

        match rx.recv_timeout(timeout) {
            Ok((status, stdout, stderr)) => {
                let (ok, error) = match status {
                    Ok(status) if status.success() => (true, None),
                    Ok(status) => (false, Some(format!("{program} exited with {status}"))),
                    Err(err) => (false, Some(format!("{program} did not finish: {err}"))),
                };
                CommandOutput {
                    ok,
                    stdout: stdout.trim().to_string(),
                    stderr: stderr.trim().to_string(),
                    error,
                }
            }
            Err(_) => CommandOutput::failed(format!(
                "{program} did not answer within {}s",
                timeout.as_secs()
            )),
        }
    }
}
