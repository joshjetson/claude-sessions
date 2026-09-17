//! The answer decision — mostly, what it refuses.
//!
//! The load-bearing assertion is that the kind comes from the RECORDED
//! notification and never from the caller. The answer command is invoked by a
//! coordinating model, and a model that supplied its own kind could label a
//! verdict a question and answer it.

use std::time::{Duration, SystemTime};

use super::{decide, open_prompt, AnswerDecision};
use crate::qarun::AnswerRefusal;
use crate::types::{
    Notification, NotificationKind, NotificationLevel, NotificationStatus, Session, SessionStatus,
};

fn notif(id: &str, ts: &str, kind: NotificationKind, status: NotificationStatus) -> Notification {
    Notification {
        id: id.to_string(),
        title: "which environment?".to_string(),
        message: String::new(),
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: Some(6688),
        level: NotificationLevel::Warn,
        kind,
        ts: ts.to_string(),
        status,
    }
}

fn session(tty: Option<&str>) -> Session {
    Session {
        session_id: "abcd".to_string(),
        pids: vec![1],
        cwd: "/repo".to_string(),
        tty: tty.map(str::to_string),
        lstart: None,
        session_file: None,
        session_mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        session_size: None,
        status: SessionStatus::Working,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: Some(6688),
    }
}

#[test]
fn the_newest_open_prompt_wins() {
    // An agent that asks twice without an answer is asking about the same
    // blockage, and the later phrasing is the one it is waiting on.
    let all = [
        notif("a", "2026-09-17T10:00:00Z", NotificationKind::Question, NotificationStatus::Unread),
        notif("b", "2026-09-17T11:00:00Z", NotificationKind::Question, NotificationStatus::Unread),
    ];
    assert_eq!(open_prompt(all.iter(), 6688).unwrap().id, "b");
}

#[test]
fn a_resolved_prompt_is_not_open() {
    let all = [notif(
        "a",
        "2026-09-17T10:00:00Z",
        NotificationKind::Question,
        NotificationStatus::Resolved,
    )];
    assert!(open_prompt(all.iter(), 6688).is_none());
}

#[test]
fn ordinary_progress_is_not_a_prompt() {
    let all = [notif(
        "a",
        "2026-09-17T10:00:00Z",
        NotificationKind::Info,
        NotificationStatus::Unread,
    )];
    assert!(open_prompt(all.iter(), 6688).is_none());
}

#[test]
fn an_answer_is_delivered_to_the_live_session() {
    let all = vec![notif("a", "t", NotificationKind::Question, NotificationStatus::Unread)];
    let live = session(Some("ttys004"));
    let sessions = vec![&live];

    match decide(&all, &sessions, 6688, "the MR preview build") {
        AnswerDecision::Deliver { session, resolve } => {
            assert_eq!(session.session_id, "abcd");
            assert_eq!(resolve, vec!["a".to_string()]);
        }
        other => panic!("expected delivery, got {other:?}"),
    }
}

#[test]
fn the_kind_comes_from_the_record_not_the_caller() {
    // The whole point. A coordinator cannot relabel a verdict as a question,
    // because it never supplies the label at all.
    let all = vec![notif("v", "t", NotificationKind::Verdict, NotificationStatus::Unread)];
    let live = session(Some("ttys004"));
    let sessions = vec![&live];

    assert_eq!(
        decide(&all, &sessions, 6688, "yes, PASS is right"),
        AnswerDecision::Refused(AnswerRefusal::VerdictIsHuman)
    );
}

#[test]
fn nothing_asked_means_nothing_to_answer() {
    // Otherwise a coordinator could type into a session that never spoke to it.
    let live = session(Some("ttys004"));
    let sessions = vec![&live];
    assert!(matches!(
        decide(&[], &sessions, 6688, "hello"),
        AnswerDecision::Refused(AnswerRefusal::UnknownKind(_))
    ));
}

#[test]
fn an_empty_answer_is_refused_before_a_session_is_needed() {
    let all = vec![notif("a", "t", NotificationKind::Question, NotificationStatus::Unread)];
    assert_eq!(
        decide(&all, &[], 6688, "   "),
        AnswerDecision::Refused(AnswerRefusal::Empty)
    );
}

#[test]
fn no_live_session_is_reported_as_such() {
    let all = vec![notif("a", "t", NotificationKind::Question, NotificationStatus::Unread)];
    assert_eq!(
        decide(&all, &[], 6688, "an answer"),
        AnswerDecision::Refused(AnswerRefusal::NoSession)
    );
}

#[test]
fn a_session_with_no_terminal_is_refused_rather_than_worked_around() {
    let all = vec![notif("a", "t", NotificationKind::Question, NotificationStatus::Unread)];
    let orphan = session(None);
    let sessions = vec![&orphan];
    assert_eq!(
        decide(&all, &sessions, 6688, "an answer"),
        AnswerDecision::Refused(AnswerRefusal::NoTty)
    );
}

#[test]
fn every_open_question_for_the_task_is_resolved_not_just_the_newest() {
    // Answering clears the blockage. Leaving older ones open would keep the row
    // asking after the reviewer was no longer needed.
    let all = vec![
        notif("a", "2026-09-17T10:00:00Z", NotificationKind::Question, NotificationStatus::Unread),
        notif("b", "2026-09-17T11:00:00Z", NotificationKind::Question, NotificationStatus::Read),
    ];
    let live = session(Some("ttys004"));
    let sessions = vec![&live];

    match decide(&all, &sessions, 6688, "answer") {
        AnswerDecision::Deliver { resolve, .. } => {
            assert_eq!(resolve.len(), 2, "older open questions were left asking");
        }
        other => panic!("expected delivery, got {other:?}"),
    }
}
