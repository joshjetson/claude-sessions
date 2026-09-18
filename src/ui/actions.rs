//! The worker that does everything the draw thread must not.
//!
//! Brief §10 mandate #9: no blocking process spawns in a render path. In the
//! Node app `qaMenuLabel` shelled out to `git` while formatting a menu row, so a
//! slow repository froze the whole dashboard. Here the UI enqueues an
//! [`Action`] and a worker thread runs it; the only thing that comes back is a
//! flash message and a refresh request.

pub mod board;
pub mod deploy;
mod services;
mod system;

pub use board::Lookup;
pub use services::{BoardServices, Sounds};
use system::{kill_pids, open_with, play, purge, ssh};

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::odoo::{Blocker, OdooProject, StageRecord, TaskDetail};
use crate::term::{Exec, LaunchRequest, SpawnPolicy, TerminalDriver};
use crate::ui::board::BoardUpdate;
use crate::ui::state::Action;
use crate::util::path_leaf;

/// The 0/400/1000ms poll after a kill. SIGTERM is not instant — the process
/// lingers in `ps` for a moment — so a single refresh would redraw the row it
/// just killed. Node polled three times; so does this.
pub const KILL_REFRESH_DELAYS: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(400),
    Duration::from_millis(1000),
];

/// How long an agent gets between the signal and its tab closing. Closing the
/// tab under a live process leaves it running headless with nowhere to report.
pub const PURGE_GRACE: Duration = Duration::from_millis(150);

/// A purge closes tabs as well as killing processes, so the last poll is later
/// than a plain kill's.
pub const PURGE_REFRESH_DELAYS: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(400),
    Duration::from_millis(1200),
];

/// What the worker sends back.
///
/// `PartialEq` but not `Eq`: a usage reading carries percentages, and a
/// percentage is a float.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionResult {
    Flash(String),
    /// Scan now.
    Refresh,
    /// A session was launched: poll faster for a while.
    Launched,
    /// A board fetch landed.
    Board(Box<BoardUpdate>),
    /// A deploy-board fetch landed.
    Deploy(Box<crate::ui::deploy::DeployUpdate>),
    /// Something a dialog was waiting on.
    Data(Box<BoardData>),
    /// A plan-usage reading this process took.
    Usage(Box<crate::usage::UsageSnapshot>),
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
    /// The recorded Optics processes for a task. `None` covers every reason
    /// there is nothing to show — not configured, project not on Optics, no
    /// recordings, the lookup failed — because the pane treats them alike.
    TaskOptics {
        task_id: i64,
        optics: Option<crate::optics::TaskOptics>,
    },
    /// Stage names for a set of tasks — what the purge dialog needs for the
    /// sessions the board's current filter does not cover.
    TaskStages(std::collections::BTreeMap<i64, String>),
    /// The QA menu label, once the QAden head probe has run. The menu opens
    /// with whatever the cache already knew and swaps this in — brief §10
    /// mandate #9: no blocking spawn while a row is being formatted.
    QaLabel {
        task_id: i64,
        label: String,
    },
    /// The current user's open merge requests, for the board tab's `M`.
    OpenMrs(Vec<crate::gitlab::OpenMr>),
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

/// Run one action exactly as the worker would. Exists so the spawn-gate tests
/// can drive the real dispatcher rather than a copy of it.
#[cfg(test)]
pub(crate) fn run_for_test(
    action: Action,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    services: &BoardServices,
    results: &Sender<ActionResult>,
) {
    run(action, driver, policy, services, results);
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
            let title = path_leaf(&cwd).to_string();
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
        Action::FetchTaskStages { task_ids } => {
            board::fetch(services, Lookup::TaskStages { task_ids }, results)
        }
        Action::Launch(spec) => board::launch(&spec, services, driver, policy, results),
        Action::SendToSession(spec) => {
            board::send_to_session(&spec, services, driver, policy, results)
        }
        Action::NudgeCoordinator(spec) => board::nudge_coordinator(&spec, driver, policy, results),
        Action::Resume(request) => board::resume(&request, services, driver, policy, results),
        Action::MoveStage {
            task_id,
            stage_id,
            stage_name,
        } => board::move_to_named_stage(services, task_id, stage_id, &stage_name, results),
        Action::OpenUrl(url) | Action::OpenPath(url) => open_with(&url, policy, results),
        Action::RefreshUsage => {
            // `$HOME` rather than the dashboard's cwd: a repository's CLAUDE.md
            // or settings must not change what a usage check reports.
            let snapshot = crate::usage::fetch_usage(
                &Exec::new(policy),
                &services.paths.home,
                crate::usage::FETCH_TIMEOUT,
            );
            let _ = results.send(ActionResult::Usage(Box::new(snapshot)));
        }
        Action::FetchTaskOptics { task_id, project } => {
            // Every failure resolves to "nothing recorded": coverage is an
            // annotation, and a pane that refuses to paint because Optics is
            // down would be worse than one that says nothing.
            let optics = services
                .optics
                .as_ref()
                .and_then(|client| client.task_optics(task_id, &project).ok().flatten());
            let _ = results.send(ActionResult::Data(Box::new(BoardData::TaskOptics {
                task_id,
                optics,
            })));
        }
        Action::RefreshQaState { task_id } => {
            services
                .qa_heads
                .refresh_for_task(&Exec::new(policy), &services.paths, task_id);
            let _ = results.send(ActionResult::Data(Box::new(BoardData::QaLabel {
                task_id,
                label: crate::qaden::menu_label_for(&services.paths, task_id, &services.qa_heads),
            })));
        }
        Action::Purge(entries) => purge(&entries, driver, policy, results),
        Action::Ssh { project } => ssh(&project, services, driver, policy, results),
        Action::Sound(level) => {
            if let Some(file) = services.sounds.file(level) {
                play(file, policy);
            }
        }
        Action::WritePipelineTemplate { repo, pipeline_id } => {
            board::write_pipeline_template(&repo, &pipeline_id, results)
        }

        // --- deploy -------------------------------------------------------------
        Action::RefreshDeploy => deploy::refresh(services, results),
        Action::MergeMrs { project, targets } => {
            deploy::merge(services, &project, &targets, results)
        }
        Action::FetchOpenMrs => deploy::open_mrs(services, results),
        // Starting and cancelling belong to the ENGINE, not to this worker: a
        // deploy has to outlive the dashboard, so the feed posts it to the
        // daemon and the loop handles the answer.
        Action::StartDeploy { .. } | Action::CancelDeploy { .. } => {}
        // The engine owns the notification list and persists it; the daemon
        // client that writes a status change through arrives with Phase 6. The
        // local copy has already been updated for immediate feedback.
        Action::Notifications { .. } => {}

        Action::Refresh | Action::RefreshBurst | Action::SelectSession { .. } => {}
    }
}

/// The command a `n` launch runs, matching what the Node app typed into a fresh
/// tab.
const LAUNCH_COMMAND: &str = "claude --dangerously-skip-permissions";
