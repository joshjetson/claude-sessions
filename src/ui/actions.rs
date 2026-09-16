//! The worker that does everything the draw thread must not.
//!
//! Brief §10 mandate #9: no blocking process spawns in a render path. In the
//! Node app `qaMenuLabel` shelled out to `git` while formatting a menu row, so a
//! slow repository froze the whole dashboard. Here the UI enqueues an
//! [`Action`] and a worker thread runs it; the only thing that comes back is a
//! flash message and a refresh request.

pub mod board;
mod services;

pub use board::Lookup;
pub use services::{BoardServices, Sounds};

use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::odoo::{Blocker, OdooProject, StageRecord, TaskDetail};
use crate::ssh::{resolve_ssh_host, ssh_command, SshResolution, SSHING_COMMAND};
use crate::term::{LaunchRequest, SpawnPolicy, TerminalDriver};
use crate::ui::board::BoardUpdate;
use crate::ui::state::Action;

/// The 0/400/1000ms poll after a kill. SIGTERM is not instant — the process
/// lingers in `ps` for a moment — so a single refresh would redraw the row it
/// just killed. Node polled three times; so does this.
pub const KILL_REFRESH_DELAYS: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(400),
    Duration::from_millis(1000),
];

/// What the worker sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    Flash(String),
    /// Scan now.
    Refresh,
    /// A session was launched: poll faster for a while.
    Launched,
    /// A board fetch landed.
    Board(Box<BoardUpdate>),
    /// Something a dialog was waiting on.
    Data(Box<BoardData>),
}

/// An answer addressed to whichever dialog asked for it.
///
/// One variant per lookup rather than a generic blob: an answer that arrives
/// after the cursor moved on must be identifiable as not-for-this-dialog, which
/// is what [`crate::ui::dialogs::Dialog::accept`] checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardData {
    Blockers {
        task_id: i64,
        blockers: Vec<Blocker>,
    },
    Stages {
        task_id: i64,
        stages: Vec<StageRecord>,
    },
    Projects(Vec<OdooProject>),
    TaskDescription {
        task_id: i64,
        detail: Option<TaskDetail>,
    },
    /// `task_id` is what the answer was about, so a failure reaches the dialog
    /// that asked rather than the one that happens to be open.
    Failed {
        task_id: Option<i64>,
        error: String,
    },
}

pub struct ActionWorker {
    actions: Sender<Action>,
    results: Receiver<ActionResult>,
    handle: Option<JoinHandle<()>>,
}

impl ActionWorker {
    pub fn start(
        driver: Arc<dyn TerminalDriver>,
        policy: SpawnPolicy,
        services: BoardServices,
    ) -> Self {
        let (actions, action_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("claude-sessions-actions".into())
            .spawn(move || {
                while let Ok(action) = action_rx.recv() {
                    run(action, &driver, policy, &services, &result_tx);
                }
            })
            .ok();
        ActionWorker {
            actions,
            results,
            handle,
        }
    }

    /// Never blocks: a full queue or a dead worker is dropped rather than
    /// stalling the frame.
    pub fn submit(&self, action: Action) {
        let _ = self.actions.send(action);
    }

    pub fn drain(&self) -> Vec<ActionResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            out.push(result);
        }
        out
    }
}

impl Drop for ActionWorker {
    fn drop(&mut self) {
        // Dropping the sender ends the worker's `recv` loop.
        let (dead, _) = mpsc::channel();
        let _ = std::mem::replace(&mut self.actions, dead);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(
    action: Action,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    services: &BoardServices,
    results: &Sender<ActionResult>,
) {
    match action {
        Action::FocusTerminal(session) => {
            let result = driver.focus(&session);
            if !result.ok {
                let reason = result.error.unwrap_or_else(|| "unknown reason".into());
                let _ = results.send(ActionResult::Flash(format!(
                    "Couldn't focus that session's terminal: {reason}"
                )));
            }
        }
        Action::LaunchSession { cwd } => {
            let title = cwd.rsplit('/').next().unwrap_or(&cwd).to_string();
            let request = LaunchRequest::new(cwd.clone(), LAUNCH_COMMAND).title(title);
            let result = driver.launch(&request);
            if result.ok {
                let _ = results.send(ActionResult::Launched);
            } else {
                let reason = result.error.unwrap_or_else(|| "unknown reason".into());
                let _ = results.send(ActionResult::Flash(format!(
                    "Couldn't open a terminal for {cwd}: {reason}"
                )));
            }
        }
        Action::Kill { pids, label } => {
            match kill_pids(&pids, policy) {
                Ok(()) => {
                    let _ = results.send(ActionResult::Flash(format!("Sent SIGTERM to {label}.")));
                }
                Err(message) => {
                    let _ = results.send(ActionResult::Flash(message));
                    return;
                }
            }
            for delay in KILL_REFRESH_DELAYS {
                if !delay.is_zero() {
                    thread::sleep(delay);
                }
                if results.send(ActionResult::Refresh).is_err() {
                    return;
                }
            }
        }
        Action::OpenEditor { .. } => {
            // Handled on the main thread: it has to take the terminal back from
            // the alternate screen first, which only the loop can do.
        }

        // --- board ------------------------------------------------------------
        Action::RefreshBoard(options) => board::refresh_board(services, &options, results),
        Action::FetchBlockers {
            task_id,
            blocker_ids,
        } => board::fetch(
            services,
            Lookup::Blockers {
                task_id,
                blocker_ids,
            },
            results,
        ),
        Action::FetchStages {
            task_id,
            project_id,
        } => board::fetch(
            services,
            Lookup::Stages {
                task_id,
                project_id,
            },
            results,
        ),
        Action::FetchProjects => board::fetch(services, Lookup::Projects, results),
        Action::FetchTaskDescription { task_id } => {
            board::fetch(services, Lookup::TaskDescription { task_id }, results)
        }
        Action::Launch(spec) => board::launch(&spec, services, driver, policy, results),
        Action::SendToSession(spec) => {
            board::send_to_session(&spec, services, driver, policy, results)
        }
        Action::Resume(request) => board::resume(&request, services, driver, policy, results),
        Action::MoveStage {
            task_id,
            stage_id,
            stage_name,
        } => board::move_to_named_stage(services, task_id, stage_id, &stage_name, results),
        Action::OpenUrl(url) => open_with(&url, policy, results),
        Action::Ssh { project } => ssh(&project, services, driver, policy, results),
        Action::Sound(level) => {
            if let Some(file) = services.sounds.file(level) {
                play(file, policy);
            }
        }
        Action::WritePipelineTemplate { repo, pipeline_id } => {
            board::write_pipeline_template(&repo, &pipeline_id, results)
        }
        // The engine owns the notification list and persists it; the daemon
        // client that writes a status change through arrives with Phase 6. The
        // local copy has already been updated for immediate feedback.
        Action::Notifications { .. } => {}

        Action::Refresh | Action::RefreshBurst | Action::SelectSession { .. } => {}
    }
}

/// `open <url>` — the one shell-out that is not a terminal driver call.
fn open_with(target: &str, policy: SpawnPolicy, results: &Sender<ActionResult>) {
    if let Err(refused) = policy.check("open a browser") {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    if let Err(error) = Command::new("open").arg(target).status() {
        let _ = results.send(ActionResult::Flash(format!(
            "Could not open {target}: {error}"
        )));
    }
}

/// A notification sound. Failure is silence, which is the correct failure mode
/// for a sound.
fn play(file: &str, policy: SpawnPolicy) {
    if policy.check("play a sound").is_err() {
        return;
    }
    let _ = Command::new("afplay").arg(file).spawn();
}

/// Open a terminal on the server a project runs on.
///
/// `~/.ssh/config` is the source of truth — the same file `sshing` reads — so a
/// host resolved here is the host you would pick there. An ambiguous match is
/// never guessed between: connecting to the wrong server is worse than not
/// connecting.
fn ssh(
    project: &str,
    services: &BoardServices,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    if let Err(refused) = policy.check(&format!("open an ssh session for {project}")) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    let config =
        crate::config::ConfigHandle::load(&services.paths, crate::config::EnvOverrides::from_env());
    let hosts = crate::ssh::load_ssh_hosts(&crate::ssh::ssh_config_path(&services.paths.home));
    let (command, say) = match resolve_ssh_host(project, &hosts, config.ssh_hosts()) {
        SshResolution::Host(host) => {
            let mut say = String::new();
            // A malformed HostName produces "could not resolve hostname", which
            // says nothing about the real cause. Say it before connecting.
            if let Some(problem) = &host.problem {
                say = format!("~/.ssh/config: {} Fix: {}", problem.message, problem.fix);
            }
            (ssh_command(&host.alias), say)
        }
        SshResolution::Ambiguous(aliases) => (
            SSHING_COMMAND.to_string(),
            format!(
                "{project} matches several hosts ({}) — opening sshing to choose.",
                aliases.join(", ")
            ),
        ),
        SshResolution::NoMatch => (
            SSHING_COMMAND.to_string(),
            format!("No ssh host matches {project} — opening sshing."),
        ),
    };
    if !say.is_empty() {
        let _ = results.send(ActionResult::Flash(say));
    }
    let home = services.paths.home.to_string_lossy().into_owned();
    let request = LaunchRequest::new(home, command.clone()).title(format!("ssh {project}"));
    let result = driver.launch(&request);
    if !result.ok {
        let reason = result.error.unwrap_or_else(|| "unknown reason".into());
        let _ = results.send(ActionResult::Flash(format!(
            "Could not open a terminal for {project}: {reason}"
        )));
    }
}

/// The command a `n` launch runs, matching what the Node app typed into a fresh
/// tab.
const LAUNCH_COMMAND: &str = "claude --dangerously-skip-permissions";

/// SIGTERM, through the same gate every other child process goes through.
///
/// `/bin/kill` rather than a raw syscall on purpose: [`SpawnPolicy`] guards
/// process *starts*, so routing the signal through one means a test run cannot
/// kill anything even by accident — which is exactly the failure the policy was
/// written for.
fn kill_pids(pids: &[u32], policy: SpawnPolicy) -> Result<(), String> {
    if pids.is_empty() {
        return Err("Nothing to kill.".to_string());
    }
    policy
        .check("kill a session")
        .map_err(|refused| refused.message)?;
    let mut command = Command::new("kill");
    command.arg("-TERM");
    for pid in pids {
        command.arg(pid.to_string());
    }
    match command.status() {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("Could not signal the session: {err}")),
    }
}
