//! The verdict a QA session parks, announced once per round.
//!
//! A QA session reports its outcome with `notify --title "QA #N: PASS"` or
//! `"QA #N: REVISION REQUIRED"`, and QAden's `/qa` posts `"QA #N: CHECKPOINT"`
//! on the way. Each post used to be its own row with its own sound, and a
//! session that revised its note posted the same verdict again: one task
//! raised four "REVISION REQUIRED" rows in an afternoon. What the reviewer
//! needs is one thing: this round has a verdict, go and post the note.
//!
//! So every such post for a task and round shares one row, with a fixed id.
//! The first verdict of a round rings. Everything after it, and every
//! checkpoint, rewrites the row in silence. A new round, read from QAden's
//! `run.json`, gets a new row and rings again.

use crate::scan::ProcessSource;
use crate::types::{Notification, NotificationKind};

use super::engine::EngineInner;
use super::notify::NewNotification;

/// What an agent's post is, read from its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPost {
    /// PASS or REVISION REQUIRED: the round's outcome.
    Verdict,
    /// Progress inside a round, which needs nobody yet.
    Checkpoint,
    /// Anything else. The role policy decides.
    Other,
}

/// Classify a post by its title, `QA #<task>: <OUTCOME>`.
///
/// Read from the title because the prompt and QAden both put the outcome
/// there, and neither sends a kind. A post sent with `--kind verdict` counts
/// as a verdict whatever its title says.
pub fn classify_post(title: &str, kind: NotificationKind) -> AgentPost {
    let outcome = title_parts(title)
        .map(|(_, outcome)| outcome.to_ascii_uppercase())
        .unwrap_or_default();
    if outcome.starts_with("PASS") || outcome.starts_with("REVISION") {
        AgentPost::Verdict
    } else if outcome.starts_with("CHECKPOINT") {
        AgentPost::Checkpoint
    } else if kind == NotificationKind::Verdict {
        AgentPost::Verdict
    } else {
        AgentPost::Other
    }
}

/// The task a `QA #<task>: ...` title names.
pub fn title_task_id(title: &str) -> Option<i64> {
    title_parts(title).map(|(task_id, _)| task_id)
}

/// `QA #<task>: <rest>` split into the task and the rest, trimmed.
fn title_parts(title: &str) -> Option<(i64, &str)> {
    let rest = title.trim_start().strip_prefix("QA #")?;
    let (digits, rest) = rest.split_once(':')?;
    let task_id = digits.trim().parse().ok()?;
    Some((task_id, rest.trim()))
}

impl<S: ProcessSource> EngineInner<S> {
    /// Raise a post from an agent (`POST /notify`).
    ///
    /// A verdict or a checkpoint that names its task shares that task's row
    /// for the round. See the module docs. Anything else goes straight
    /// through the role policy.
    pub(crate) fn raise_agent_post(&self, mut new: NewNotification) -> Option<Notification> {
        let post = classify_post(&new.title, new.kind);
        let task_id = new.task_id.or_else(|| title_task_id(&new.title));
        let (AgentPost::Verdict | AgentPost::Checkpoint, Some(task_id)) = (post, task_id) else {
            return self.raise_notification(new);
        };
        // QAden archives a finished round before it starts the next, so the
        // round read here is the one this post is about.
        let round = crate::qaden::qa_run_state(&self.paths, task_id, |_| None).round;
        let announced_key = format!("verdict:{task_id}:{round}");
        let announced = self.db.was_alerted(&announced_key);
        // A checkpoint after the verdict gets its own silent row. Written into
        // the verdict's row, it would replace the one line the reviewer needs.
        let row = match (post, announced) {
            (AgentPost::Checkpoint, true) => format!("checkpoint-{task_id}-r{round}"),
            _ => format!("verdict-{task_id}-r{round}"),
        };
        let rings = post == AgentPost::Verdict && !announced;
        if rings {
            self.db.mark_alerted(&announced_key);
        }
        if post == AgentPost::Verdict {
            new.kind = NotificationKind::Verdict;
        }
        new.source = "verdict";
        new.task_id = Some(task_id);
        new.id = Some(row);
        new.silent = !rings;
        self.raise_notification(new)
    }
}
