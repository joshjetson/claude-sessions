//! Supervising a project's production deploy.
//!
//! The child belongs to the ENGINE, not to the dashboard that asked for it.
//! That is the whole reason the daemon exists: closing the TUI in the middle of
//! a production deploy used to kill it, because the child was the TUI's.
//!
//! Three rules are load-bearing and each is a scar:
//!
//! * The command runs under `/bin/sh -lc` — a LOGIN shell. `nvm`, `asdf`,
//!   `gcloud` and `kubectl` all live behind profile setup, and a non-login
//!   shell finds none of them.
//! * Output is held in a ring buffer. A chatty deploy (docker build, gradle)
//!   emits tens of thousands of lines, the pane only ever shows a screenful,
//!   and keeping the whole log for hours is what grows the heap.
//! * Only exit 0 closes tasks out, and only tasks whose merge request actually
//!   MERGED. A failed deploy shipped nothing, and an open MR is not in the
//!   branch that was built however far along the board the task looks.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::scan::ProcessSource;
use crate::types::{DeployRun, DeployRunStatus};
use crate::util::iso_now;

use super::engine::{Engine, EngineInner};
use super::events::EngineEvent;
use super::notify::ActionResult;

mod complete;

/// How many output lines a run keeps. Anything older has scrolled out of every
/// pane that could show it.
pub const MAX_RUN_LINES: usize = 2000;
/// How many go to a client. The pane shows a screenful; the rest are available
/// on demand from `GET /deploy/log/:project`.
pub const WIRE_RUN_LINES: usize = 200;
/// The login shell the command runs under, and the flag that makes it one.
pub const DEPLOY_SHELL: &str = "/bin/sh";
pub const DEPLOY_SHELL_FLAGS: &str = "-lc";

/// A deploy the engine is supervising, as the engine holds it.
///
/// The public [`DeployRun`] is what leaves; this keeps the full ring buffer and
/// the pid, neither of which a client has any use for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRunState {
    pub command: String,
    pub status: DeployRunStatus,
    pub exit_code: Option<i32>,
    pub started_at: String,
    /// Capped at [`MAX_RUN_LINES`]; `total` counts everything ever emitted.
    lines: VecDeque<String>,
    total: usize,
    /// The child's pid while it runs, so cancelling and shutdown can signal it.
    pub pid: Option<u32>,
}

impl DeployRunState {
    pub fn started(command: impl Into<String>, pid: Option<u32>) -> DeployRunState {
        DeployRunState {
            command: command.into(),
            status: DeployRunStatus::Running,
            exit_code: None,
            started_at: iso_now(),
            lines: VecDeque::new(),
            total: 0,
            pid,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status == DeployRunStatus::Running
    }

    pub(crate) fn push(&mut self, line: String) {
        self.lines.push_back(line);
        self.total += 1;
        while self.lines.len() > MAX_RUN_LINES {
            self.lines.pop_front();
        }
    }

    /// Everything still held, oldest first — what `GET /deploy/log/:project`
    /// answers with.
    pub fn log(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    /// The wire form: the trailing window plus how much there was in total, so
    /// a pane can say "showing the last 200 of 9,412".
    pub fn wire(&self, project: &str) -> DeployRun {
        DeployRun {
            project: project.to_string(),
            command: self.command.clone(),
            status: self.status,
            exit_code: self.exit_code,
            started_at: self.started_at.clone(),
            lines: self
                .lines
                .iter()
                .skip(self.lines.len().saturating_sub(WIRE_RUN_LINES))
                .cloned()
                .collect(),
            total_lines: self.total,
        }
    }
}

// --- what a client asks for --------------------------------------------------

impl<S: ProcessSource + Send + 'static> Engine<S> {
    /// Start a project's deploy command.
    ///
    /// Refused — never an error — when the project has no command, when one is
    /// already running, or when this process may not spawn at all.
    pub fn start_deploy(&self, project: &str) -> ActionResult {
        let Some(config) = self.inner.config().deploy_project_config(project) else {
            return ActionResult::failed(format!(
                "\"{project}\" is not configured for deploy — add \
                 deploy.projects[\"{project}\"] to ~/.claude-sessions.json."
            ));
        };
        if config.command.is_empty() {
            return ActionResult::failed(format!(
                "No deploy command configured for \"{project}\" — set \
                 deploy.projects[\"{project}\"].command in ~/.claude-sessions.json."
            ));
        }
        // The configured key, not what the caller typed: every later lookup
        // (the board, the runs map, the log route) is keyed by it.
        let project = config.project.clone();
        if self
            .inner
            .state()
            .deploy_runs
            .get(&project)
            .is_some_and(DeployRunState::is_running)
        {
            return ActionResult::failed(format!("{project} is already deploying."));
        }
        if let Err(refused) = self.inner.spawn.check(&format!("deploy {project}")) {
            return ActionResult::failed(refused.message);
        }
        // Everything above this point is configuration, and a platform that
        // cannot run the command can still answer for it correctly. What it
        // cannot do is the line below: `cmd /C` is not a login shell and has no
        // equivalent, so the command would run against a different PATH than
        // the one it was written for. See [`crate::platform::LOGIN_SHELL`].
        if !crate::platform::LOGIN_SHELL {
            return ActionResult::failed(crate::platform::unsupported(&format!(
                "Deploying {project}"
            )));
        }

        // A login shell so the command sees the PATH the user's own terminal
        // has; the cwd is the project's repository when config names one.
        let mut command = Command::new(DEPLOY_SHELL);
        command
            .arg(DEPLOY_SHELL_FLAGS)
            .arg(&config.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return ActionResult::failed(format!(
                    "Could not start the {project} deploy: {error}"
                ))
            }
        };

        let pid = child.id();
        self.inner.state().deploy_runs.insert(
            project.clone(),
            DeployRunState::started(config.command.clone(), Some(pid)),
        );
        self.inner.publish_run(&project);

        // Both pipes feed one channel, so the interleaving a terminal would
        // show is the interleaving the pane shows. A child whose pipe fills up
        // blocks until somebody drains it, which is why these read eagerly
        // rather than only at exit.
        let (tx, rx) = mpsc::channel();
        let mut pipes: Vec<Box<dyn Read + Send>> = Vec::with_capacity(2);
        if let Some(pipe) = child.stdout.take() {
            pipes.push(Box::new(pipe));
        }
        if let Some(pipe) = child.stderr.take() {
            pipes.push(Box::new(pipe));
        }
        for pipe in pipes {
            let tx = tx.clone();
            let _ = thread::Builder::new()
                .name("claude-sessions-deploy-out".into())
                .spawn(move || pump(pipe, &tx));
        }
        drop(tx);

        self.inner.spawn_worker(move |inner| {
            // Ends when both readers have hit EOF, which is what makes the
            // wait below immediate rather than a race.
            for line in rx {
                inner.push_deploy_line(&project, line);
            }
            let code = child.wait().ok().and_then(|status| status.code());
            inner.finish_deploy(&project, code);
        });
        ActionResult::ok()
    }

    /// SIGTERM a running deploy. The engine owns the child, so the engine is
    /// what does the killing.
    pub fn cancel_deploy(&self, project: &str) -> ActionResult {
        let pid = self
            .inner
            .state()
            .deploy_runs
            .get(project)
            .filter(|run| run.is_running())
            .and_then(|run| run.pid);
        match pid {
            Some(pid) => {
                self.inner.terminate(pid);
                ActionResult::ok()
            }
            None => ActionResult::failed(format!("No running deploy for {project}.")),
        }
    }

    /// The whole ring buffer — everything the engine still holds.
    pub fn deploy_log(&self, project: &str) -> Vec<String> {
        self.inner
            .state()
            .deploy_runs
            .get(project)
            .map(DeployRunState::log)
            .unwrap_or_default()
    }

    pub fn deploy_run(&self, project: &str) -> Option<DeployRun> {
        self.inner
            .state()
            .deploy_runs
            .get(project)
            .map(|run| run.wire(project))
    }

    /// Reload the deploy board off the caller's thread.
    ///
    /// Fire-and-forget like `refresh_board`: the query is one Odoo round trip
    /// plus a `glab` read per merge request, and an HTTP route must not wait
    /// for it. MANUAL only — nothing polls this, because every refresh spends
    /// a GitLab API call per open MR.
    pub fn refresh_deploy_board(&self) {
        if self.inner.fetch_deploy.is_none() {
            return;
        }
        self.inner.spawn_worker(|inner| {
            inner.poll_deploy();
        });
    }
}

/// Read one stream into lines. `from_utf8_lossy` rather than a `lines()`
/// iterator: build output routinely carries ANSI escapes and the occasional
/// invalid byte, and one of those must not end the capture.
fn pump(pipe: Box<dyn Read + Send>, tx: &Sender<String>) {
    let mut reader = BufReader::new(pipe);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        if tx
            .send(String::from_utf8_lossy(&buffer).into_owned())
            .is_err()
        {
            return;
        }
    }
}

// --- the engine side ---------------------------------------------------------

impl<S: ProcessSource> EngineInner<S> {
    pub(crate) fn publish_run(&self, project: &str) {
        let run = self
            .state()
            .deploy_runs
            .get(project)
            .map(|run| run.wire(project));
        if let Some(run) = run {
            self.publish(EngineEvent::DeployRun {
                project: project.to_string(),
                run: Box::new(run),
            });
        }
    }

    pub(crate) fn push_deploy_line(&self, project: &str, line: String) {
        if let Some(run) = self.state().deploy_runs.get_mut(project) {
            run.push(line.clone());
        }
        // Line by line rather than a whole run: the output pane is what the
        // user is watching, and batching it would make a deploy look hung.
        self.publish(EngineEvent::DeployOutput {
            project: project.to_string(),
            line,
        });
    }

    /// Append a line the engine itself wrote — the completion report — so it
    /// lands in the same pane as the command's own output.
    fn report(&self, project: &str, line: impl Into<String>) {
        self.push_deploy_line(project, line.into());
    }

    /// SIGTERM, through the same gate every other child goes through.
    ///
    /// `/bin/kill` rather than a raw syscall: [`SpawnPolicy`](crate::term::SpawnPolicy)
    /// guards process STARTS, so routing the signal through one means a test
    /// run cannot kill anything even by accident.
    pub(crate) fn terminate(&self, pid: u32) {
        if !crate::platform::PROCESS_SIGNALS {
            return;
        }
        if self.spawn.check("stop a deploy").is_err() {
            return;
        }
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Every deploy still running. `Engine::stop` signals each one — which is
    /// what makes the shutdown dialog's warning true.
    pub(crate) fn terminate_deploys(&self) {
        let pids: Vec<u32> = self
            .state()
            .deploy_runs
            .values()
            .filter(|run| run.is_running())
            .filter_map(|run| run.pid)
            .collect();
        for pid in pids {
            self.terminate(pid);
        }
    }
}

/// Every run as a client sees it.
pub(crate) fn wire_runs(
    runs: &std::collections::BTreeMap<String, DeployRunState>,
) -> std::collections::BTreeMap<String, DeployRun> {
    runs.iter()
        .map(|(project, run)| (project.clone(), run.wire(project)))
        .collect()
}

/// The names of the deploys that are actually RUNNING, sorted.
///
/// What the shutdown dialog warns about — and what it must not pad with
/// finished ones, or the warning becomes noise nobody reads. Written once and
/// taken by iterator because the engine keeps its runs in a `BTreeMap` and the
/// dashboard in a `HashMap`.
pub fn running_deploys<'a>(
    runs: impl IntoIterator<Item = (&'a String, &'a DeployRun)>,
) -> Vec<String> {
    let mut names: Vec<String> = runs
        .into_iter()
        .filter(|(_, run)| run.is_running())
        .map(|(project, _)| project.clone())
        .collect();
    names.sort_unstable();
    names
}
