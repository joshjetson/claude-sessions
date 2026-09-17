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

/// How much of a child's output is kept. `glab api` on a busy project can
/// answer with megabytes; the Node wrapper capped `maxBuffer` at the same size
/// and this is that cap, applied per stream.
pub const MAX_OUTPUT: usize = 20 * 1024 * 1024;

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

/// One command to run: the program, its argv, and the three things a caller
/// occasionally needs to change about the environment it runs in.
///
/// An options struct rather than a second `run_with_env_and_cwd` (WORKING.md
/// rule 5): the terminal drivers want none of it, the `glab` wrapper wants the
/// host in the child's environment, and `git` wants a working directory.
#[derive(Debug, Clone)]
pub struct ExecRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub timeout: Duration,
    pub cwd: Option<&'a Path>,
    /// Added to the inherited environment, not replacing it.
    pub env: &'a [(String, String)],
}

impl<'a> ExecRequest<'a> {
    pub fn new(program: &'a str, args: &'a [String], timeout: Duration) -> Self {
        ExecRequest {
            program,
            args,
            timeout,
            cwd: None,
            env: &[],
        }
    }

    pub fn in_dir(mut self, cwd: Option<&'a Path>) -> Self {
        self.cwd = cwd;
        self
    }

    pub fn with_env(mut self, env: &'a [(String, String)]) -> Self {
        self.env = env;
        self
    }
}

/// Anything that can run a command. The one seam a test substitutes: the
/// `glab` wrapper is built on argv builders plus this, so its retry rules are
/// exercised with a recorded script and no process anywhere near the test.
pub trait Runner: Send + Sync {
    fn run_request(&self, request: &ExecRequest<'_>) -> CommandOutput;
}

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
        self.run_request(&ExecRequest::new(program, args, timeout))
    }
}

impl Runner for Exec {
    fn run_request(&self, request: &ExecRequest<'_>) -> CommandOutput {
        let ExecRequest {
            program,
            args,
            timeout,
            cwd,
            env,
        } = *request;
        if let Err(refused) = self.policy.check(&format!("run {program}")) {
            return CommandOutput::failed(refused.message);
        }

        let mut command = Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
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
            if let Some(pipe) = child.stdout.take() {
                let _ = pipe.take(MAX_OUTPUT as u64).read_to_string(&mut stdout);
            }
            if let Some(pipe) = child.stderr.take() {
                let _ = pipe.take(MAX_OUTPUT as u64).read_to_string(&mut stderr);
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
