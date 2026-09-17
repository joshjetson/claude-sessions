//! The launch queue: which live session belongs to which task we just started.
//!
//! One entry per launch, never one slot. Eight tasks were once started from one
//! folder inside a minute; a `claude` process writes no transcript for its
//! first seconds, so the scanner showed each as a `starting-<pid>` placeholder
//! with no file and no task id. That placeholder satisfied every launch, the
//! single pending slot held only the newest, and nothing re-checked the link
//! once the real transcript arrived — seven tasks ended up pointing at one
//! placeholder.
//!
//! Hence the three rules in [`claimable`], each of which is one of those
//! failures written down.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::db::TaskSession;
use crate::scan::ProcessSource;
use crate::types::Session;
use crate::util::{is_within_dir, iso_now, same_dir, trim_trailing_separators};

use super::engine::EngineInner;
use super::events::{wire_session, EngineEvent};
use super::state::{PendingLaunch, SessionIndex, TaskLink, TaskLinkPatch, TaskLinkStatus};

/// What the launch flow tells the engine to watch for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingRequest {
    pub cwd: String,
    pub task_id: Option<i64>,
    /// The sessions that already existed when the launch went out.
    pub known_session_ids: Vec<String>,
}

impl<S: ProcessSource> EngineInner<S> {
    /// Queue a launch. A re-launch replaces that task's own entry and leaves
    /// every other task's alone — wiping the queue here would abandon a session
    /// spawned twenty seconds earlier.
    pub(crate) fn set_pending(&self, request: PendingRequest) {
        let launch = PendingLaunch {
            cwd: request.cwd,
            task_id: request.task_id,
            launched_at: self.now(),
            known_session_ids: request.known_session_ids.into_iter().collect(),
        };
        let mut state = self.state();
        if let Some(task_id) = launch.task_id {
            state
                .pending
                .retain(|queued| queued.task_id != Some(task_id));
        }
        state.pending.push(launch);
    }

    /// Record (or update) the session working a task.
    pub(crate) fn link_task(&self, task_id: i64, patch: TaskLinkPatch) -> TaskLink {
        let link = self.state().link_task(task_id, patch);
        self.publish(EngineEvent::TaskLinked {
            task_id,
            info: link.clone(),
        });
        link
    }

    /// Match every queued launch against the sessions this tick found.
    pub(crate) fn link_pending_sessions(&self, sessions: &SessionIndex, now: SystemTime) {
        let mut linked: Vec<(Option<i64>, Session)> = Vec::new();
        {
            let mut state = self.state();
            state.prune_pending(now);
            if state.pending.is_empty() {
                return;
            }

            // Every session another task already holds. A session belongs to
            // one task, so a launch may never claim one that is spoken for.
            let mut taken: HashMap<String, i64> = state
                .task_sessions
                .iter()
                .filter(|(_, link)| !link.session_id.is_empty())
                .map(|(task_id, link)| (link.session_id.clone(), *task_id))
                .collect();

            let mut still_waiting: Vec<PendingLaunch> = Vec::new();
            for launch in std::mem::take(&mut state.pending) {
                let Some(session) = self.claim(&launch, sessions, &taken) else {
                    still_waiting.push(launch);
                    continue;
                };
                if let Some(task_id) = launch.task_id {
                    let existing_cwd = state
                        .task_sessions
                        .get(&task_id)
                        .map(|link| link.cwd.clone())
                        .unwrap_or_default();
                    state.link_task(
                        task_id,
                        TaskLinkPatch {
                            session_id: Some(session.session_id.clone()),
                            session_file: session.session_file.clone(),
                            // The recorded directory wins: it is where the task
                            // was launched, which a `--resume` in a worktree
                            // would otherwise overwrite.
                            cwd: Some(if existing_cwd.is_empty() {
                                session.cwd.clone()
                            } else {
                                existing_cwd
                            }),
                            status: Some(TaskLinkStatus::Running),
                            stage_id: None,
                        },
                    );
                    taken.insert(session.session_id.clone(), task_id);
                }
                linked.push((launch.task_id, session.clone()));
            }
            state.pending = still_waiting;
        }

        for (task_id, session) in linked {
            // The 4b contract: every live link reaches the index, or the next
            // round looks for this task's transcript by brute force.
            if let (Some(task_id), Some(file)) = (task_id, session.session_file.as_ref()) {
                self.db.put_task_session(&TaskSession {
                    task_id,
                    session_file: file.to_string_lossy().into_owned(),
                    cwd: session.cwd.clone(),
                    updated_at: iso_now(),
                });
            }
            self.publish(EngineEvent::SessionLinked(Box::new(wire_session(&session))));
        }
    }

    /// The live session a single queued launch started, or `None` while it
    /// waits. Three passes, in the Node order.
    fn claim<'a>(
        &self,
        launch: &PendingLaunch,
        sessions: &'a SessionIndex,
        taken: &HashMap<String, i64>,
    ) -> Option<&'a Session> {
        let wanted = trim_trailing_separators(&launch.cwd);
        if wanted.is_empty() {
            return None;
        }
        let fresh = |session: &Session| !launch.known_session_ids.contains(&session.session_id);
        let here = |session: &Session| same_dir(&session.cwd, wanted);
        let under = |session: &Session| is_within_dir(&session.cwd, wanted);
        let ok = |session: &&Session| self.claimable(session, launch, taken);

        sessions
            .iter()
            .find(|session| here(session) && fresh(session) && ok(session))
            // Deliberately accepts a session that was already running:
            // `--resume` reuses its session id, so it is not new to us.
            .or_else(|| sessions.iter().find(|session| here(session) && ok(session)))
            .or_else(|| {
                sessions
                    .iter()
                    .find(|session| under(session) && fresh(session) && ok(session))
            })
    }

    /// The three hard rules.
    fn claimable(
        &self,
        session: &Session,
        launch: &PendingLaunch,
        taken: &HashMap<String, i64>,
    ) -> bool {
        // 1. A process that has not written its transcript yet appears as a
        //    `starting-<pid>` placeholder with no file and no task id. It
        //    matches every launch in the folder, so eight launches in one
        //    folder all claimed the same placeholder and nothing ever corrected
        //    them. Wait for the real transcript instead: the poller runs twice
        //    a second, and an unlinked task is a far smaller fault than a task
        //    linked to another task's work.
        let Some(session_file) = session.session_file.as_ref() else {
            return false;
        };
        // 2. A session whose own transcript names a different task cannot be
        //    the one this launch just started, however well its directory
        //    matches.
        if let (Some(wanted), Some(named)) = (launch.task_id, session.task_id) {
            if wanted != named {
                return false;
            }
        }
        // 3. Nor one that another task already holds — in this daemon's memory,
        //    or in the index, which is the only one of the two that survives a
        //    restart.
        let owner = taken.get(&session.session_id).copied().or_else(|| {
            self.db
                .task_for_session_file(&session_file.to_string_lossy())
                .map(|row| row.task_id)
        });
        match (owner, launch.task_id) {
            (None, _) => true,
            (Some(owner), Some(wanted)) => owner == wanted,
            (Some(_), None) => false,
        }
    }
}
