//! The `glab` wrapper. Nothing here starts a process: the argv builders are
//! pure, and the client runs against [`FakeRunner`], which records what it was
//! asked and answers from a script.

mod argv;
mod client;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::term::{CommandOutput, ExecRequest, Runner};

use super::Gitlab;

/// One command the client ran, flattened to the parts a test asserts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

/// A [`Runner`] that never spawns. Replies are consumed in order; the last one
/// repeats, which keeps "call it twice and check the retry" tests short.
#[derive(Default)]
pub(crate) struct FakeRunner {
    replies: Mutex<Vec<CommandOutput>>,
    calls: Mutex<Vec<Call>>,
}

impl FakeRunner {
    pub(crate) fn new(replies: Vec<CommandOutput>) -> Arc<FakeRunner> {
        Arc::new(FakeRunner {
            replies: Mutex::new(replies),
            calls: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Runner for FakeRunner {
    fn run_request(&self, request: &ExecRequest<'_>) -> CommandOutput {
        self.calls.lock().unwrap().push(Call {
            program: request.program.to_string(),
            args: request.args.to_vec(),
            cwd: request.cwd.map(|path| path.to_string_lossy().into_owned()),
            env: request.env.to_vec(),
            timeout: request.timeout,
        });
        let mut replies = self.replies.lock().unwrap();
        if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies.first().cloned().unwrap_or_else(|| ok(""))
        }
    }
}

pub(crate) fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        ok: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
        error: None,
    }
}

pub(crate) fn json_ok(value: Value) -> CommandOutput {
    ok(&value.to_string())
}

pub(crate) fn failed(stderr: &str) -> CommandOutput {
    CommandOutput {
        ok: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
        error: Some("glab exited with 1".to_string()),
    }
}

/// A configured client over a scripted runner. The recheck delay is collapsed
/// so the retry test costs nothing.
pub(crate) fn client(replies: Vec<CommandOutput>) -> (Gitlab, Arc<FakeRunner>) {
    let runner = FakeRunner::new(replies);
    let gitlab = Gitlab::new(
        Some("git.example.com".to_string()),
        Arc::clone(&runner) as Arc<dyn Runner>,
    )
    .with_recheck_delay(Duration::from_millis(0));
    (gitlab, runner)
}
