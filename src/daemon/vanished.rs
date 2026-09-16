//! Archiving a task session that went away without saying so.
//!
//! Two things used to have to hold for this to happen: the agent had to run the
//! done hook, and — failing that — the in-memory link had to still hold the
//! session. The first is a prompt instruction a project can override away — two
//! projects had — and the second is daemon memory, empty after a restart and
//! blank for anything the dashboard did not spawn. Eight sessions ended with no
//! archive at all, including a whole epic's worth of work and two
//! merge-conflict sessions, whose prompt never mentions the done hook by
//! design.
//!
//! Sessions now carry a task id read from their own transcript, so a
//! disappearing session can be archived on that alone. The link is used when
//! present, as it also knows the working directory.

use std::path::{Path, PathBuf};

use crate::archive::ArchiveRequest;
use crate::db::TaskSession;
use crate::scan::ProcessSource;
use crate::transcript::TaskRefCache;
use crate::util::iso_now;

use super::engine::EngineInner;
use super::events::EngineEvent;
use super::state::{SeenTaskSession, SessionIndex, TaskLinkStatus};

impl<S: ProcessSource> EngineInner<S> {
    /// Archive the transcript of any task session that has gone away, and keep
    /// the index pointed at the ones that are still here.
    ///
    /// Three passes: what disappeared since the last tick, what is live now,
    /// and the older link-based path for a transcript that names no task at
    /// all.
    pub(crate) fn auto_archive_vanished_sessions(
        &self,
        sessions: &SessionIndex,
        refs: &mut TaskRefCache,
    ) {
        let vanished: Vec<(String, SeenTaskSession)> = {
            let mut state = self.state();
            let gone: Vec<(String, SeenTaskSession)> = state
                .seen_task_sessions
                .iter()
                .filter(|(session_id, _)| !sessions.contains(session_id))
                .map(|(session_id, info)| (session_id.clone(), info.clone()))
                .collect();
            for (session_id, _) in &gone {
                state.seen_task_sessions.remove(session_id);
            }
            gone
        };

        for (session_id, info) in vanished {
            self.archive_ended_session(
                info.task_id,
                &session_id,
                &info.cwd,
                Some(&info.session_file),
                refs,
            );
        }

        // Remember the current ones for the next tick — and point the index at
        // them. The index is the 4b contract: without a row here, a task whose
        // work moved to a QA worktree is looked for by brute force, and after a
        // restart not found at all.
        {
            let mut state = self.state();
            for session in sessions.iter() {
                let (Some(task_id), Some(file)) = (session.task_id, session.session_file.clone())
                else {
                    continue;
                };
                let seen = SeenTaskSession {
                    task_id,
                    cwd: session.cwd.clone(),
                    session_file: file,
                };
                if state.seen_task_sessions.get(&session.session_id) == Some(&seen) {
                    continue;
                }
                self.db.put_task_session(&TaskSession {
                    task_id,
                    session_file: seen.session_file.to_string_lossy().into_owned(),
                    cwd: seen.cwd.clone(),
                    updated_at: iso_now(),
                });
                state
                    .seen_task_sessions
                    .insert(session.session_id.clone(), seen);
            }
        }

        // The original link-based path still runs, for sessions whose transcript
        // carries no task reference but which the dashboard spawned itself.
        let orphaned: Vec<(i64, String, String, Option<PathBuf>)> = {
            let state = self.state();
            state
                .task_sessions
                .iter()
                .filter(|(_, link)| link.is_running() && !link.session_id.is_empty())
                .filter(|(_, link)| !sessions.contains(&link.session_id))
                .map(|(task_id, link)| {
                    (
                        *task_id,
                        link.session_id.clone(),
                        link.cwd.clone(),
                        link.session_file.clone(),
                    )
                })
                .collect()
        };
        for (task_id, session_id, cwd, file) in orphaned {
            self.archive_ended_session(task_id, &session_id, &cwd, file.as_deref(), refs);
        }
    }

    /// Archive one ended session, unless the archive already names it.
    ///
    /// Archive EVERY round, not the first one. A task that goes through two or
    /// three QA rounds writes a new transcript each time, and skipping on "this
    /// task already has an archive" froze the archive at round 1 — one task
    /// kept a 167-line ready-check stub instead of its real QA session. The
    /// session id is the thing to compare: re-archiving the SAME session is the
    /// only work worth skipping.
    fn archive_ended_session(
        &self,
        task_id: i64,
        session_id: &str,
        cwd: &str,
        session_file: Option<&Path>,
        refs: &mut TaskRefCache,
    ) {
        let archive = self.archive();
        if archive
            .task_meta(task_id)
            .is_some_and(|meta| meta.session_id == session_id)
        {
            return;
        }
        let request = ArchiveRequest {
            cwd: cwd.to_string(),
            alt_cwd: String::new(),
            session_id: session_id.to_string(),
            session_file: session_file.map(|path| path.to_path_buf()),
        };
        if archive
            .archive_task_conversation(task_id, &request, refs)
            .is_none()
        {
            return;
        }
        {
            let mut state = self.state();
            state.archived_tasks.insert(task_id);
            if let Some(link) = state.task_sessions.get_mut(&task_id) {
                link.status = Some(TaskLinkStatus::Ended);
            }
        }
        self.publish(EngineEvent::TaskArchived {
            task_id,
            session_id: session_id.to_string(),
        });
    }
}
