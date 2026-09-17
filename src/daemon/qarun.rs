//! The daemon's half of a QA run: deciding whether an answer may be delivered.
//!
//! The daemon does not type into terminals — in this port only the UI's action
//! worker drives one. So this resolves the decision and hands back the session
//! to type into; the caller performs the send.
//!
//! # Where the kind comes from
//!
//! Not from the caller. The answer command is invoked by a coordinating model,
//! and a model that supplies its own `--kind` could mark a verdict as a
//! question and answer it. The kind is read from the RECORDED notification that
//! raised the question, which the coordinator did not write.
//!
//! That is stronger than the Node app, where the flag is taken at face value,
//! and it is the difference between a rule and a request.

use crate::qarun::{deliverable_session, AnswerRefusal};
use crate::types::{Notification, NotificationKind, NotificationStatus, Session};

/// What the daemon decided about one answer.
#[derive(Debug, Clone, PartialEq)]
pub enum AnswerDecision<'a> {
    /// Type this into that session, then resolve these notifications.
    Deliver {
        session: &'a Session,
        resolve: Vec<String>,
    },
    Refused(AnswerRefusal),
}

/// The newest unanswered question or verdict raised for a task.
///
/// Newest wins: an agent that asks twice without an answer is asking about the
/// same blockage, and the later phrasing is the one it is waiting on.
pub fn open_prompt<'a>(
    notifications: impl IntoIterator<Item = &'a Notification>,
    task_id: i64,
) -> Option<&'a Notification> {
    notifications
        .into_iter()
        .filter(|n| {
            n.task_id == Some(task_id)
                && n.status != NotificationStatus::Resolved
                && matches!(
                    n.kind,
                    NotificationKind::Question | NotificationKind::Verdict
                )
        })
        .max_by(|a, b| a.ts.cmp(&b.ts))
}

/// Decide whether an answer may be delivered, and to which session.
///
/// `sessions` is every live session on that task, newest first.
pub fn decide<'a>(
    notifications: &'a [Notification],
    sessions: &'a [&'a Session],
    task_id: i64,
    answer: &str,
) -> AnswerDecision<'a> {
    let open = open_prompt(notifications.iter(), task_id);

    // No recorded prompt means nothing asked, so there is nothing to answer.
    // Treating that as a question would let a coordinator type into a session
    // that never spoke to it.
    let Some(open) = open else {
        return AnswerDecision::Refused(AnswerRefusal::UnknownKind(NotificationKind::Info));
    };

    match deliverable_session(open.kind, answer, sessions.first().copied()) {
        Ok(session) => AnswerDecision::Deliver {
            session,
            // Every open prompt for the task, not just the newest: answering
            // clears the blockage, and leaving older ones unresolved would keep
            // the row asking after the reviewer was no longer needed.
            resolve: notifications
                .iter()
                .filter(|n| {
                    n.task_id == Some(task_id)
                        && n.status != NotificationStatus::Resolved
                        && n.kind == NotificationKind::Question
                })
                .map(|n| n.id.clone())
                .collect(),
        },
        Err(refusal) => AnswerDecision::Refused(refusal),
    }
}

#[cfg(test)]
mod tests;
