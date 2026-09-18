//! What a coordinator may answer — and mostly, what it may not.
//!
//! The prompt tells a coordinator not to answer a verdict checkpoint. These
//! assertions are why that instruction is worth anything: a prompt is a
//! request, and a coordinator that talks itself into answering one meets a
//! refusal here rather than a QA session that quietly takes its word for a PASS.

use std::time::{Duration, SystemTime};

use crate::qarun::{deliverable_session, may_answer, AnswerRefusal, MAX_ANSWER_CHARS};
use crate::types::{NotificationKind, Session, SessionStatus};

fn session(tty: Option<&str>) -> Session {
    Session {
        session_id: "abcd1234".to_string(),
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
        task_id: Some(4101),
        run_id: None,
    }
}

#[test]
fn a_question_with_a_real_answer_is_deliverable() {
    assert_eq!(
        may_answer(NotificationKind::Question, "the preview build for that MR"),
        Ok(())
    );
}

#[test]
fn a_verdict_is_never_deliverable() {
    assert_eq!(
        may_answer(NotificationKind::Verdict, "looks right to me"),
        Err(AnswerRefusal::VerdictIsHuman)
    );
}

#[test]
fn a_verdict_is_refused_even_when_the_answer_agrees() {
    // "Only confirming what it already said" is the most persuasive version of
    // this mistake, so it gets its own assertion.
    assert_eq!(
        may_answer(NotificationKind::Verdict, "yes, PASS is correct"),
        Err(AnswerRefusal::VerdictIsHuman)
    );
}

#[test]
fn an_unremarkable_kind_is_refused_rather_than_assumed_to_be_a_question() {
    // The safe default when you cannot tell what you are answering is not to
    // answer it. A missing kind means a sender that predates the split.
    assert_eq!(
        may_answer(NotificationKind::Info, "anything"),
        Err(AnswerRefusal::UnknownKind(NotificationKind::Info))
    );
}

#[test]
fn an_empty_answer_is_refused() {
    assert_eq!(
        may_answer(NotificationKind::Question, "   "),
        Err(AnswerRefusal::Empty)
    );
}

#[test]
fn an_answer_that_is_really_a_pasted_report_is_refused() {
    let long = "x".repeat(MAX_ANSWER_CHARS + 1);
    assert_eq!(
        may_answer(NotificationKind::Question, &long),
        Err(AnswerRefusal::TooLong(MAX_ANSWER_CHARS + 1))
    );
}

#[test]
fn an_answer_exactly_at_the_cap_is_allowed() {
    let exact = "x".repeat(MAX_ANSWER_CHARS);
    assert_eq!(may_answer(NotificationKind::Question, &exact), Ok(()));
}

#[test]
fn every_refusal_explains_itself() {
    // A coordinator told only "no" retries differently. One told why escalates.
    for refusal in [
        AnswerRefusal::VerdictIsHuman,
        AnswerRefusal::UnknownKind(NotificationKind::Info),
        AnswerRefusal::Empty,
        AnswerRefusal::TooLong(9999),
        AnswerRefusal::NoSession,
        AnswerRefusal::NoTty,
    ] {
        assert!(refusal.detail().len() > 10, "thin refusal: {refusal:?}");
    }
}

#[test]
fn the_policy_is_checked_before_any_session_is_looked_for() {
    // So a verdict is refused identically whether or not one is live.
    let live = session(Some("ttys004"));
    assert_eq!(
        deliverable_session(NotificationKind::Verdict, "PASS", Some(&live)),
        Err(AnswerRefusal::VerdictIsHuman)
    );
    assert_eq!(
        deliverable_session(NotificationKind::Verdict, "PASS", None),
        Err(AnswerRefusal::VerdictIsHuman)
    );
}

#[test]
fn no_session_is_reported_as_such() {
    assert_eq!(
        deliverable_session(NotificationKind::Question, "x", None),
        Err(AnswerRefusal::NoSession)
    );
}

#[test]
fn a_session_with_no_terminal_is_reported_not_worked_around() {
    // Opening a new tab would start a second process against the same
    // conversation.
    for tty in [None, Some(""), Some("??"), Some("-")] {
        let orphan = session(tty);
        assert_eq!(
            deliverable_session(NotificationKind::Question, "x", Some(&orphan)),
            Err(AnswerRefusal::NoTty),
            "tty {tty:?} should not be typeable"
        );
    }
}

#[test]
fn a_real_session_is_returned_for_delivery() {
    let live = session(Some("ttys004"));
    assert_eq!(
        deliverable_session(NotificationKind::Question, "x", Some(&live)),
        Ok(&live)
    );
}
