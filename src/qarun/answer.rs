//! What a coordinating session may answer on the reviewer's behalf.
//!
//! The prompt tells a coordinator not to answer a verdict checkpoint. This
//! module is why that instruction is worth anything: a prompt is a request, and
//! a coordinator that talks itself into answering one meets a refusal here
//! rather than a QA session that quietly takes its word for a PASS.
//!
//! The asymmetry is deliberate. A coordinator that wrongly escalates costs the
//! reviewer a question to read. A coordinator that wrongly answers sends a QA
//! pass down a false trail, and the pass has no way to know it should doubt the
//! answer it was given.

use crate::types::{NotificationKind, Session};

/// Longer than this and it is a pasted report, not an answer.
pub const MAX_ANSWER_CHARS: usize = 2000;

/// Why an answer will not be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerRefusal {
    /// A verdict checkpoint. QAden stops `/qa` there by design, and answering
    /// it removes that checkpoint without anyone deciding to.
    VerdictIsHuman,
    /// Anything that is not an explicit question. The safe default when you
    /// cannot tell what you are answering is not to answer it.
    UnknownKind(NotificationKind),
    Empty,
    TooLong(usize),
    /// Nothing is working that task.
    NoSession,
    /// The session has no controlling terminal, so it cannot be typed into at
    /// all. Reported rather than worked around: opening a new tab would start a
    /// second process against the same conversation.
    NoTty,
}

impl AnswerRefusal {
    pub fn detail(&self) -> String {
        match self {
            AnswerRefusal::VerdictIsHuman => {
                "A verdict checkpoint is never answered by a coordinator. Escalate it unchanged."
                    .to_string()
            }
            AnswerRefusal::UnknownKind(kind) => format!(
                "Refusing to answer something of kind \"{}\". Only an explicit question may be answered.",
                kind.as_str()
            ),
            AnswerRefusal::Empty => "An empty answer is not an answer.".to_string(),
            AnswerRefusal::TooLong(len) => format!(
                "Answers are capped at {MAX_ANSWER_CHARS} characters and this one is {len}. \
                 Anything longer belongs in the task, not typed into a session."
            ),
            AnswerRefusal::NoSession => "No live session for that task.".to_string(),
            AnswerRefusal::NoTty => {
                "That session has no terminal to type into.".to_string()
            }
        }
    }
}

/// Whether this is something a coordinator may answer.
///
/// Checked before any session is looked for, so a verdict is refused identically
/// whether or not one happens to be live.
pub fn may_answer(kind: NotificationKind, answer: &str) -> Result<(), AnswerRefusal> {
    if kind == NotificationKind::Verdict {
        return Err(AnswerRefusal::VerdictIsHuman);
    }
    if !kind.is_answerable() {
        return Err(AnswerRefusal::UnknownKind(kind));
    }
    if answer.trim().is_empty() {
        return Err(AnswerRefusal::Empty);
    }
    if answer.chars().count() > MAX_ANSWER_CHARS {
        return Err(AnswerRefusal::TooLong(answer.chars().count()));
    }
    Ok(())
}

/// The session an answer would be typed into, once the policy allows it.
pub fn deliverable_session<'a>(
    kind: NotificationKind,
    answer: &str,
    session: Option<&'a Session>,
) -> Result<&'a Session, AnswerRefusal> {
    may_answer(kind, answer)?;
    let session = session.ok_or(AnswerRefusal::NoSession)?;
    match session.tty.as_deref() {
        Some(tty) if !tty.is_empty() && tty != "??" && tty != "-" => Ok(session),
        _ => Err(AnswerRefusal::NoTty),
    }
}
