//! Which live session belongs to a task, and where a row sends you.
//!
//! Ported from the Node app's `src/tui/controller.js`. That file kept THREE
//! copies of "the newest live session for this task" — `findTaskLiveSession`,
//! `findTaskLiveSessions` and `liveSessionsForTask` in `actions.js` — held
//! apart only to dodge an import cycle between the controller and the actions.
//! Here there is one [`task_sessions`] and everything else is a parameter on
//! it (WORKING.md rule 5).
//!
//! # Why the transcript wins over the launch link
//!
//! `session.task_id` is read out of each transcript by the scanner, so it
//! survives a daemon restart and covers sessions started outside the dashboard.
//! The in-memory `task_sessions` link is only a fallback: relying on it alone is
//! why the board reported "no live session" for tasks that plainly had one.

use std::time::SystemTime;

use crate::daemon::TaskLink;
use crate::types::Session;
use crate::util::{parse_timestamp, same_dir, trim_trailing_separators};

/// Every live session working a task, most recently active FIRST.
///
/// Several can race on one task — four did on one real board — so this returns
/// all of them and callers that want "the" session take the head.
pub fn task_sessions<'a>(
    sessions: impl Iterator<Item = &'a Session>,
    task_id: i64,
) -> Vec<&'a Session> {
    let mut found: Vec<&Session> = sessions
        .filter(|session| session.task_id == Some(task_id))
        .collect();
    found.sort_by_key(|session| std::cmp::Reverse(activity_at(session)));
    found
}

/// The single live session working a task, or nothing.
///
/// The transcript's own task id first; then the recorded launch link, which is
/// what covers a session started by hand (its transcript names no task).
pub fn task_session<'a>(
    sessions: impl Iterator<Item = &'a Session> + Clone,
    task_id: i64,
    link: Option<&TaskLink>,
) -> Option<&'a Session> {
    if let Some(newest) = task_sessions(sessions.clone(), task_id).into_iter().next() {
        return Some(newest);
    }
    let link = link?;
    sessions.into_iter().find(|session| {
        (!link.session_id.is_empty() && session.session_id == link.session_id)
            || (link.session_file.is_some() && session.session_file == link.session_file)
    })
}

/// When a session was last doing something.
///
/// The transcript's own last timestamp beats the file's mtime: a transcript
/// copied or touched by an archive pass has a fresh mtime and nothing new in
/// it, and picking that one hands the revision to the wrong agent.
fn activity_at(session: &Session) -> SystemTime {
    session
        .last_timestamp
        .as_deref()
        .and_then(parse_timestamp)
        .map(SystemTime::from)
        .unwrap_or(session.session_mtime)
}

/// Where `g` (go to the conversation) and `G` (raise the terminal) land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// Switch to the sessions view and open this conversation.
    Session {
        session_id: String,
        session_file: Option<std::path::PathBuf>,
        project: String,
    },
    /// Found, but with no controlling terminal to raise. Names the session
    /// rather than implying there is none — a detached agent is still running.
    NoTerminal {
        session_id: String,
    },
    None,
}

/// The sessions-view target for a task, if it has one.
pub fn go_to_task_session<'a>(
    sessions: impl Iterator<Item = &'a Session> + Clone,
    task_id: i64,
    link: Option<&TaskLink>,
) -> SessionTarget {
    match task_session(sessions, task_id, link) {
        Some(session) => SessionTarget::Session {
            session_id: session.session_id.clone(),
            session_file: session.session_file.clone(),
            project: crate::util::project_name(&session.cwd),
        },
        None => SessionTarget::None,
    }
}

/// What `G` does: the terminal to raise, plus how many other sessions are
/// racing on the same task (which is worth saying out loud).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusTarget {
    pub session: crate::term::SessionRef,
    pub session_id: String,
    pub others: usize,
}

pub fn focus_task_terminal<'a>(
    sessions: impl Iterator<Item = &'a Session> + Clone,
    task_id: i64,
    link: Option<&TaskLink>,
) -> Result<FocusTarget, SessionTarget> {
    let all = task_sessions(sessions.clone(), task_id);
    let Some(session) = task_session(sessions, task_id, link) else {
        return Err(SessionTarget::None);
    };
    let reference = crate::term::SessionRef::from_session(session);
    if reference.tty_device().is_none() {
        return Err(SessionTarget::NoTerminal {
            session_id: session.session_id.clone(),
        });
    }
    Ok(FocusTarget {
        session: reference,
        session_id: session.session_id.clone(),
        others: all.len(),
    })
}

/// The live session a notification came from.
///
/// `cwd` alone is ambiguous when several sessions run in one folder, so the
/// session id the notification carries wins, then the task it belongs to, and
/// only then the directory.
pub fn resolve_notif_session<'a>(
    sessions: impl Iterator<Item = &'a Session> + Clone,
    notification: &crate::types::Notification,
    link: Option<&TaskLink>,
) -> Option<&'a Session> {
    if let Some(wanted) = &notification.session_id {
        if let Some(found) = sessions.clone().find(|s| &s.session_id == wanted) {
            return Some(found);
        }
    }
    if let Some(task_id) = notification.task_id {
        if let Some(found) = task_session(sessions.clone(), task_id, link) {
            return Some(found);
        }
    }
    match_session_by_cwd(sessions, &notification.cwd)
}

/// Last resort, and deliberately last: a folder can hold several sessions.
pub fn match_session_by_cwd<'a>(
    sessions: impl Iterator<Item = &'a Session>,
    cwd: &str,
) -> Option<&'a Session> {
    let target = trim_trailing_separators(cwd);
    if target.is_empty() {
        return None;
    }
    sessions.into_iter().find(|s| same_dir(&s.cwd, target))
}
