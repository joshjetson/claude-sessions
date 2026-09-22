//! Admission: which tasks in a run may start right now.
//!
//! This holds no model, and that placement is the point of the module. A
//! coordinating session can decide what to ask, what to answer and what to
//! escalate. It must not decide how many sessions may run at once, because a
//! backstop a model can raise is not a backstop.
//!
//! Everything here is pure. Every refusal names itself, because a run that
//! starts nothing looks exactly like a run that is merely slow, and "nothing
//! happened" is the failure mode this whole feature exists to remove.

use std::collections::HashSet;

use super::QaRun;
use crate::qaden::{QaRunState, QaVerdict};

/// Why a task may not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not part of this run.
    NotInRun(i64),
    /// A session is already working it. Two passes on one task share a worktree
    /// path, and the worktree setup tears the existing checkout down to rebuild
    /// it — mid-review, for the other session.
    AlreadyRunning(i64),
    /// This run started it once already.
    AlreadySpawned(i64),
    /// It reached a verdict. Starting again archives a completed round, which
    /// is what the QA menu label exists to stop a person doing by accident — a
    /// scheduler must not do it by accident either.
    AlreadyFinished(i64, QaVerdict),
    /// Every lane is busy.
    LaneLimit { running: usize, limit: usize },
}

impl Refusal {
    /// A sentence to put in front of a person.
    pub fn detail(&self) -> String {
        match self {
            Refusal::NotInRun(id) => format!("Task {id} is not part of this run."),
            Refusal::AlreadyRunning(id) => format!("Task {id} already has a live session."),
            Refusal::AlreadySpawned(id) => format!("This run already started task {id}."),
            Refusal::AlreadyFinished(id, verdict) => {
                format!(
                    "Task {id} already reached a verdict ({}).",
                    verdict.as_str()
                )
            }
            Refusal::LaneLimit { running, limit } => {
                format!("{running} of {limit} lanes busy.")
            }
        }
    }
}

/// What the scheduler needs to know that is not in the run itself.
pub struct AdmitCtx<'a> {
    /// Task ids with a live session right now.
    pub live_task_ids: &'a HashSet<i64>,
    /// A task's recorded QA state. Injected so admission can be tested without
    /// a QA directory on disk.
    pub state_of: &'a dyn Fn(i64) -> QaRunState,
    /// `None` means uncapped.
    pub lane_limit: Option<usize>,
}

impl AdmitCtx<'_> {
    fn limit(&self, run: &QaRun) -> Option<usize> {
        run.lane_limit.or(self.lane_limit)
    }
}

/// Whether one task may start now.
pub fn admit(run: &QaRun, task_id: i64, ctx: &AdmitCtx<'_>) -> Result<(), Refusal> {
    if !run.task_ids.contains(&task_id) {
        return Err(Refusal::NotInRun(task_id));
    }
    if ctx.live_task_ids.contains(&task_id) {
        return Err(Refusal::AlreadyRunning(task_id));
    }
    if run.spawned.contains(&task_id) {
        return Err(Refusal::AlreadySpawned(task_id));
    }
    // A verdict refuses a fresh pass only while it still describes the work in
    // front of you.
    //
    // `run.json` is not cleared between QA cycles, so a task that passed, went
    // out, came back revised and returned to the stage still carries its old
    // verdict. One such file was eleven days old and blocked its task from ever
    // starting in a run — refused silently, with the row reading "not running".
    //
    // `stale` is already computed: both commits known and different means the
    // developer pushed since, so the verdict is about code that no longer
    // exists. When it cannot be computed — no worktree, no recorded head, which
    // is the case for every older file — we do not know, and the safer default
    // is to let the pass run. A redundant pass costs a session; a refused one
    // costs a task nobody notices is missing.
    let state = (ctx.state_of)(task_id);
    if let Some(verdict) = state.verdict {
        let judgeable = state.head.is_some() && state.current_head.is_some();
        if judgeable && !state.stale {
            return Err(Refusal::AlreadyFinished(task_id, verdict));
        }
    }
    if let Some(limit) = ctx.limit(run) {
        let running = run
            .task_ids
            .iter()
            .filter(|id| ctx.live_task_ids.contains(id))
            .count();
        if running >= limit {
            return Err(Refusal::LaneLimit { running, limit });
        }
    }
    Ok(())
}

/// The tasks a run should start next, in the run's own order, up to the free
/// lanes.
///
/// Returned rather than started, so a caller can log the plan before acting on
/// it. The run itself is not mutated: planning is not starting.
pub fn plan_spawns(run: &QaRun, ctx: &AdmitCtx<'_>) -> Vec<i64> {
    // A local copy, because each planned task occupies a lane for the rest of
    // this plan. Without that, one pass hands the same single free lane to
    // every remaining task and starts all of them.
    let mut occupied = ctx.live_task_ids.clone();
    let mut plan = Vec::new();

    for &task_id in &run.task_ids {
        let local = AdmitCtx {
            live_task_ids: &occupied,
            state_of: ctx.state_of,
            lane_limit: ctx.lane_limit,
        };
        match admit(run, task_id, &local) {
            Ok(()) => {
                plan.push(task_id);
                occupied.insert(task_id);
            }
            // A full lane stops the plan; anything else skips this task and
            // keeps looking, since a finished task does not consume a lane.
            Err(Refusal::LaneLimit { .. }) => break,
            Err(_) => continue,
        }
    }
    plan
}

/// The first reason nothing could start, for a caller that has an empty plan
/// and has to say why.
pub fn first_refusal(run: &QaRun, ctx: &AdmitCtx<'_>) -> Option<Refusal> {
    run.task_ids
        .iter()
        .find_map(|&task_id| admit(run, task_id, ctx).err())
}
