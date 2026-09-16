//! The worker that does everything the draw thread must not.
//!
//! Brief §10 mandate #9: no blocking process spawns in a render path. In the
//! Node app `qaMenuLabel` shelled out to `git` while formatting a menu row, so a
//! slow repository froze the whole dashboard. Here the UI enqueues an
//! [`Action`] and a worker thread runs it; the only thing that comes back is a
//! flash message and a refresh request.

use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::term::{LaunchRequest, SpawnPolicy, TerminalDriver};
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
}

pub struct ActionWorker {
    actions: Sender<Action>,
    results: Receiver<ActionResult>,
    handle: Option<JoinHandle<()>>,
}

impl ActionWorker {
    pub fn start(driver: Arc<dyn TerminalDriver>, policy: SpawnPolicy) -> Self {
        let (actions, action_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("claude-sessions-actions".into())
            .spawn(move || {
                while let Ok(action) = action_rx.recv() {
                    run(action, &driver, policy, &result_tx);
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
        Action::Refresh | Action::RefreshBurst | Action::SelectSession { .. } => {}
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
