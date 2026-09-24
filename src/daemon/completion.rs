//! What happens when a task finishes.
//!
//! The orchestration is here and stays here: which order things happen in, what
//! is still done when a step fails, what the user is told afterwards. What is
//! NOT here is anything that talks to Odoo or GitLab — that is
//! [`super::backend`]'s [`TaskBackend`], and the daily-log hook Phase 11 wires
//! up. With no backend installed [`super::backend::NullBackend`] makes every
//! remote step a soft no-op and the local half — archiving, the state change,
//! the notification — still runs.
//!
//! The order matters and is the Node original's: the merge request first (so
//! the comment can link it), then the stage move, then the comment, then the
//! archive, then the log, then the notification that reports what actually
//! happened. A step that fails is reported IN that notification rather than
//! swallowed — a task once sat in "In Progress" with a completion comment on it
//! and nothing explaining the mismatch.

use std::path::PathBuf;

use crate::archive::ArchiveRequest;
use crate::scan::ProcessSource;
use crate::types::NotificationLevel;

use super::backend::{MergeRequestRequest, StageMove, StageMoveRequest, TaskDetail};
use super::engine::EngineInner;
use super::events::EngineEvent;
use super::markers::{BlockedMarker, DoneMarker};
use super::notify::NewNotification;
use super::state::{BlockedTask, TaskLinkPatch, TaskLinkStatus};
use super::summary::summary_to_html;
use crate::util::iso_now;

/// One line of the standup log. Phase 11 owns the writing; this is what it is
/// handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyLogRecord {
    pub task_id: i64,
    pub title: String,
    pub summary: String,
    pub mr_url: Option<String>,
}

/// The slot Phase 11 fills.
pub type DailyLogHook = Box<dyn Fn(&DailyLogRecord) + Send + Sync>;

impl<S: ProcessSource> EngineInner<S> {
    /// An agent signed a task off.
    pub(crate) fn process_done(&self, marker: DoneMarker) {
        if marker.task_id == 0 {
            return;
        }
        let task_id = marker.task_id;

        // What we know, without asking: the board row if it is loaded, and the
        // link if this daemon launched the session.
        let (known, link_cwd) = {
            let state = self.state();
            (
                state.task(task_id).cloned(),
                state
                    .task_sessions
                    .get(&task_id)
                    .map(|link| link.cwd.clone())
                    .unwrap_or_default(),
            )
        };
        let detail = match known {
            Some(task) => TaskDetail {
                project_id: Some(task.project_id),
                stage_id: Some(task.stage_id),
                name: task.name,
                project_name: task.project_name,
            },
            None => self.backend.task_detail(task_id).unwrap_or_default(),
        };

        let work_cwd = if link_cwd.is_empty() {
            marker.cwd.clone()
        } else {
            link_cwd
        };
        let repo_path = (!work_cwd.is_empty()).then(|| PathBuf::from(&work_cwd));

        let mr_url = repo_path
            .as_ref()
            .and_then(|cwd| {
                self.backend
                    .ensure_merge_request(&MergeRequestRequest {
                        task_id,
                        cwd: cwd.clone(),
                        project_id: detail.project_id,
                        project_name: detail.project_name.clone(),
                    })
                    .ok()
            })
            .flatten();

        // Where the finished task goes is defined by the project's pipeline, so
        // the flow shown under `P` is the one that runs.
        let moved = self.backend.move_to_stage(&StageMoveRequest {
            project_id: detail.project_id,
            stage_id: detail.stage_id,
            project_name: detail.project_name.clone(),
            repo_path: repo_path.clone(),
            preferred: self
                .config()
                .done_stage()
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            ..StageMoveRequest::to_done(task_id)
        });

        let comment = completion_comment(mr_url.as_deref(), &marker.summary, &work_cwd);
        let _ = self.backend.post_comment(task_id, &comment);

        // `alt_cwd` is where the agent signed off from, which beats the recorded
        // link when the two disagree — the link is in-memory and can be wrong;
        // the agent running the done hook in a directory is a fact.
        let archived = {
            let mut scan = self.scan();
            self.archive()
                .archive_task_conversation(
                    task_id,
                    &ArchiveRequest {
                        cwd: work_cwd.clone(),
                        alt_cwd: marker.cwd.clone(),
                        session_id: String::new(),
                        session_file: None,
                    },
                    scan.scanner.task_refs(),
                )
                .is_some()
        };

        if let Some(hook) = &self.daily_log {
            hook(&DailyLogRecord {
                task_id,
                title: detail.name.clone(),
                summary: marker.summary.clone(),
                mr_url: mr_url.clone(),
            });
        }

        {
            let mut state = self.state();
            state.blocked_tasks.remove(&task_id);
            state.done_tasks.insert(task_id);
            if archived {
                state.archived_tasks.insert(task_id);
            }
            if let Some(link) = state.task_sessions.get_mut(&task_id) {
                link.status = Some(TaskLinkStatus::Done);
            }
        }

        self.publish(EngineEvent::TaskDone {
            task_id,
            name: detail.name.clone(),
            project: detail.project_name.clone(),
            mr_url: mr_url.clone(),
        });

        self.raise_notification(NewNotification {
            cwd: marker.cwd.clone(),
            project: Some(detail.project_name.clone()),
            task_id: Some(task_id),
            level: NotificationLevel::Success,
            ..NewNotification::new(
                "done",
                match detail.name.is_empty() {
                    true => format!("✅ Task #{task_id} complete"),
                    false => format!("✅ Task #{task_id} complete: {}", detail.name),
                },
                // What actually happened, not what was attempted.
                completion_message(&detail.project_name, mr_url.as_deref(), &moved, archived),
            )
        });
        // Phase 9b refreshes the board here, so the moved task shows in its new
        // stage without waiting out the 45s poll.
    }

    /// The readiness gate stopped a task.
    pub(crate) fn process_blocked(&self, marker: BlockedMarker) {
        if marker.task_id == 0 {
            return;
        }
        let task_id = marker.task_id;
        let (name, project) = {
            let mut state = self.state();
            state.blocked_tasks.insert(
                task_id,
                BlockedTask {
                    questions: marker.questions.clone(),
                    ts: iso_now(),
                },
            );
            if state.task_sessions.contains_key(&task_id) {
                state.link_task(
                    task_id,
                    TaskLinkPatch {
                        status: Some(TaskLinkStatus::Blocked),
                        ..TaskLinkPatch::default()
                    },
                );
            }
            match state.task(task_id) {
                Some(task) => (task.name.clone(), task.project_name.clone()),
                None => (String::new(), String::new()),
            }
        };

        self.publish(EngineEvent::TaskBlocked {
            task_id,
            questions: marker.questions.clone(),
            project: project.clone(),
        });

        let questions = if marker.questions.is_empty() {
            "See the task for clarifying questions.".to_string()
        } else {
            marker
                .questions
                .iter()
                .map(|q| format!("• {q}"))
                .collect::<Vec<_>>()
                .join("  ")
        };
        self.raise_notification(NewNotification {
            cwd: marker.cwd.clone(),
            project: Some(project.clone()),
            task_id: Some(task_id),
            level: NotificationLevel::Warn,
            ..NewNotification::new(
                "blocked",
                match name.is_empty() {
                    true => format!("🚧 Task #{task_id} needs info"),
                    false => format!("🚧 Task #{task_id} needs info: {name}"),
                },
                format!(
                    "{}Readiness gate paused this task. {questions}",
                    prefix(&project)
                ),
            )
        });
    }
}

fn prefix(project: &str) -> String {
    if project.is_empty() {
        String::new()
    } else {
        format!("{project} — ")
    }
}

/// The chatter comment. The summary goes through [`summary_to_html`], which
/// escapes before it formats, so an agent's sign-off cannot inject markup.
fn completion_comment(mr_url: Option<&str>, summary: &str, cwd: &str) -> String {
    let mut html = String::from("<p>✅ <b>Completed via Claude Sessions dashboard</b></p>");
    match mr_url {
        Some(url) => html.push_str(&format!("<p>📋 <a href=\"{url}\">Merge Request</a></p>")),
        None => html.push_str("<p>⚠ No merge request detected.</p>"),
    }
    html.push_str(&summary_to_html(summary));
    if !cwd.is_empty() {
        html.push_str(&format!("<p>From <code>{cwd}</code>.</p>"));
    }
    html
}

/// What the completion notification says — every clause of it is something that
/// either happened or did not.
fn completion_message(
    project: &str,
    mr_url: Option<&str>,
    moved: &Result<StageMove, String>,
    archived: bool,
) -> String {
    let mr = match mr_url {
        Some(url) => format!("MR opened: {url}"),
        None => "no MR detected".to_string(),
    };
    let stage = match moved {
        Ok(StageMove::Moved(stage)) => format!("Moved to {stage}. "),
        Ok(StageMove::Unchanged(stage)) => format!("Already in {stage}. "),
        Ok(StageMove::NoStage(reason)) => format!("NOT moved ({reason}). "),
        Ok(StageMove::Disabled) => String::new(),
        Err(error) => format!("NOT moved ({error}). "),
    };
    let archive = if archived {
        "Transcript archived; press v on the task to resume for a revision."
    } else {
        "No transcript archived — v will start a fresh session rather than resume."
    };
    format!("{}{mr}. {stage}{archive}", prefix(project))
}
