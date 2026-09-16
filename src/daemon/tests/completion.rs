//! The completion flow, and the markdown-to-HTML conversion the Odoo comment
//! goes through. `summary_to_html` is ported from the `summaryToHtml` block of
//! `test/daemon.test.js`.

use std::collections::BTreeMap;

use super::*;
use crate::daemon::{
    summary_to_html, BlockedMarker, DoneMarker, EngineEvent, StageMove, TaskLink, TaskLinkStatus,
};
use crate::types::{Board, BoardProject, BoardStage, NotificationLevel};

// --- summary_to_html --------------------------------------------------------

#[test]
fn renders_bold_code_paragraphs_and_bullets() {
    let html = summary_to_html("**Root cause:** the `guard` ran early.\n\n- one\n- two");
    assert!(html.starts_with("<p>📝 <b>Summary</b></p>"), "{html}");
    assert!(html.contains("<b>Root cause:</b>"), "{html}");
    assert!(html.contains("<code>guard</code>"), "{html}");
    assert!(html.contains("<ul><li>one</li><li>two</li></ul>"), "{html}");
}

#[test]
fn escapes_html_so_a_summary_cannot_inject_markup_into_odoo() {
    let html = summary_to_html("fixed <script>alert(1)</script> & more");
    assert!(
        !html.contains("<script>"),
        "raw script tag survived escaping"
    );
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&amp;"));
}

#[test]
fn escaping_happens_before_formatting_so_tags_cannot_be_smuggled_in() {
    // The only tags in the output are the ones the converter put there.
    let html = summary_to_html("**<b>bold</b>** and `<i>code</i>`");
    assert_eq!(html.matches("<b>").count(), 2, "{html}"); // the heading and ours
    assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"), "{html}");
    assert!(
        html.contains("<code>&lt;i&gt;code&lt;/i&gt;</code>"),
        "{html}"
    );
}

#[test]
fn empty_input_yields_empty_output() {
    assert_eq!(summary_to_html(""), "");
    assert_eq!(summary_to_html("   "), "");
    assert_eq!(summary_to_html("\n\n"), "");
}

#[test]
fn lines_of_one_paragraph_are_joined_with_breaks() {
    let html = summary_to_html("one\ntwo\n\nthree");
    assert!(html.contains("<p>one<br>two</p>"), "{html}");
    assert!(html.contains("<p>three</p>"), "{html}");
}

#[test]
fn an_unpaired_marker_stays_literal() {
    let html = summary_to_html("2 ** 3 is not bold, nor is a lone ` backtick");
    let body = html
        .strip_prefix("<p>📝 <b>Summary</b></p>")
        .expect("the heading is always first");
    assert!(!body.contains("<b>"), "{body}");
    assert!(!body.contains("<code>"), "{body}");
    assert!(body.contains("2 ** 3"), "{body}");
}

#[test]
fn a_dash_that_is_not_a_bullet_stays_prose() {
    let html = summary_to_html("-40 degrees");
    assert!(!html.contains("<ul>"), "{html}");
    assert!(html.contains("<p>-40 degrees</p>"), "{html}");
}

// --- process_done -----------------------------------------------------------

fn a_board(task: Task) -> Board {
    let stage = BoardStage {
        stage_id: task.stage_id,
        sequence: 1,
        tasks: vec![task.clone()],
    };
    let project = BoardProject {
        project_id: task.project_id,
        stages: BTreeMap::from([(task.stage_name.clone(), stage)]),
    };
    Board {
        projects: BTreeMap::from([(task.project_name.clone(), project)]),
        task_count: 1,
        truncated: false,
    }
}

fn done(task_id: i64, cwd: &str, summary: &str) -> DoneMarker {
    DoneMarker {
        task_id,
        cwd: cwd.to_string(),
        summary: summary.to_string(),
        ts: crate::util::iso_now(),
    }
}

#[test]
fn a_completion_archives_comments_logs_and_reports_what_happened() {
    let mut harness = engine();
    let session = harness.transcript(6137, "/repo/app");
    *harness.backend.mr_url.lock().unwrap() = Some("https://git.example.com/mr/7".to_string());
    *harness.backend.stage.lock().unwrap() =
        Some(StageMove::Moved("Quality Assurance".to_string()));
    harness.state().set_board(Some(a_board(Task {
        id: 6137,
        name: "Widget alignment".to_string(),
        project_id: 11,
        project_name: "Project A".to_string(),
        stage_id: 3,
        stage_name: "In Progress".to_string(),
        ..Task::default()
    })));
    let events = harness.engine.subscribe();

    harness
        .inner()
        .process_done(done(6137, "/repo/app", "**Fix:** tightened the `guard`."));

    // The chatter comment carries the MR, the escaped summary and the folder.
    let comment = harness.backend.comment().expect("no comment posted");
    assert!(
        comment.contains("https://git.example.com/mr/7"),
        "{comment}"
    );
    assert!(comment.contains("<b>Fix:</b>"), "{comment}");
    assert!(comment.contains("<code>guard</code>"), "{comment}");
    assert!(comment.contains("<code>/repo/app</code>"), "{comment}");

    // The transcript was archived, from the folder the agent signed off in.
    assert_eq!(
        archived_session_id(&harness, 6137).as_deref(),
        Some(session.session_id.as_str())
    );

    // The standup log was handed the record; Phase 11 writes it.
    let logged = harness.daily_log.lock().unwrap().clone();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].task_id, 6137);
    assert_eq!(logged[0].title, "Widget alignment");
    assert_eq!(
        logged[0].mr_url.as_deref(),
        Some("https://git.example.com/mr/7")
    );

    // State and the feed agree about what happened.
    {
        let state = harness.state();
        assert!(state.done_tasks.contains(&6137));
        assert!(state.archived_tasks.contains(&6137));
        let notification = &state.notifications[0];
        assert_eq!(notification.level, NotificationLevel::Success);
        assert!(
            notification.title.contains("Widget alignment"),
            "{}",
            notification.title
        );
        assert!(
            notification.message.contains("Moved to Quality Assurance"),
            "{}",
            notification.message
        );
        assert!(
            notification.message.contains("Transcript archived"),
            "{}",
            notification.message
        );
    }

    let done_event = events
        .try_iter()
        .find_map(|event| match event {
            EngineEvent::TaskDone { task_id, name, .. } => Some((task_id, name)),
            _ => None,
        })
        .expect("no task-done event");
    assert_eq!(done_event, (6137, "Widget alignment".to_string()));
}

#[test]
fn a_stage_that_could_not_be_resolved_is_reported_not_swallowed() {
    // A task once sat in In Progress with a completion comment on it and
    // nothing explaining the mismatch.
    let harness = engine();
    *harness.backend.stage.lock().unwrap() = Some(StageMove::NoStage(
        "no QA-like stage in this project".to_string(),
    ));
    harness
        .inner()
        .process_done(done(6138, "/repo/app", "done"));

    let state = harness.state();
    let message = &state.notifications[0].message;
    assert!(message.contains("NOT moved (no QA-like stage"), "{message}");
    assert!(
        message.contains("No transcript archived"),
        "a task with no transcript must say so: {message}"
    );
}

#[test]
fn a_pipeline_with_no_move_step_says_nothing_about_stages() {
    let harness = engine();
    // NullBackend-equivalent: the recorder's default answer is Disabled.
    harness
        .inner()
        .process_done(done(6139, "/repo/app", "done"));
    let state = harness.state();
    let message = &state.notifications[0].message;
    assert!(!message.contains("Moved"), "{message}");
    assert!(!message.contains("NOT moved"), "{message}");
}

#[test]
fn a_task_the_board_has_not_loaded_is_looked_up() {
    let harness = engine();
    *harness.backend.detail.lock().unwrap() = crate::daemon::TaskDetail {
        project_id: Some(11),
        stage_id: Some(3),
        name: "Fetched name".to_string(),
        project_name: "Project A".to_string(),
    };
    harness.inner().process_done(done(6140, "/repo/app", ""));

    assert!(harness
        .backend
        .calls()
        .contains(&super::BackendCall::Detail(6140)));
    assert!(harness.state().notifications[0]
        .title
        .contains("Fetched name"));
}

#[test]
fn the_board_row_is_used_without_asking_odoo_again() {
    let harness = engine();
    harness.state().set_board(Some(a_board(Task {
        id: 6141,
        name: "Known".to_string(),
        project_id: 11,
        project_name: "Project A".to_string(),
        stage_id: 3,
        stage_name: "In Progress".to_string(),
        ..Task::default()
    })));
    harness.inner().process_done(done(6141, "/repo/app", ""));
    assert!(
        !harness
            .backend
            .calls()
            .contains(&super::BackendCall::Detail(6141)),
        "asked Odoo for a task the board already had"
    );
}

#[test]
fn the_recorded_working_directory_beats_the_markers_when_the_two_disagree() {
    let harness = engine();
    harness.state().task_sessions.insert(
        6142,
        TaskLink {
            cwd: "/repo/worktree".to_string(),
            session_id: "sess".to_string(),
            status: Some(TaskLinkStatus::Running),
            ..TaskLink::default()
        },
    );
    harness
        .inner()
        .process_done(done(6142, "/tmp/elsewhere", ""));

    let merge = harness
        .backend
        .calls()
        .into_iter()
        .find_map(|call| match call {
            super::BackendCall::MergeRequest(request) => Some(request),
            _ => None,
        });
    assert_eq!(
        merge.map(|request| request.cwd),
        Some(std::path::PathBuf::from("/repo/worktree"))
    );
    assert_eq!(
        harness.state().task_sessions[&6142].status,
        Some(TaskLinkStatus::Done)
    );
}

#[test]
fn a_marker_with_no_task_id_does_nothing() {
    let harness = engine();
    harness.inner().process_done(done(0, "/repo/app", "x"));
    assert!(harness.state().notifications.is_empty());
    assert!(harness.backend.calls().is_empty());
}

// --- process_blocked --------------------------------------------------------

#[test]
fn the_readiness_gate_records_the_questions_and_says_so() {
    let harness = engine();
    harness.state().task_sessions.insert(
        6143,
        TaskLink {
            session_id: "sess".to_string(),
            status: Some(TaskLinkStatus::Running),
            ..TaskLink::default()
        },
    );
    let events = harness.engine.subscribe();

    harness.inner().process_blocked(BlockedMarker {
        task_id: 6143,
        cwd: "/repo/app".to_string(),
        questions: vec![
            "Which environment?".to_string(),
            "Whose account?".to_string(),
        ],
        ts: String::new(),
    });

    {
        let state = harness.state();
        let blocked = state.blocked_tasks.get(&6143).expect("not recorded");
        assert_eq!(blocked.questions.len(), 2);
        assert!(!blocked.ts.is_empty());
        assert_eq!(
            state.task_sessions[&6143].status,
            Some(TaskLinkStatus::Blocked)
        );
        let notification = &state.notifications[0];
        assert_eq!(notification.level, NotificationLevel::Warn);
        assert!(
            notification.message.contains("• Which environment?"),
            "{}",
            notification.message
        );
    }

    assert!(events
        .try_iter()
        .any(|event| matches!(event, EngineEvent::TaskBlocked { task_id: 6143, .. })));
}

#[test]
fn a_gate_with_no_questions_still_points_at_the_task() {
    let harness = engine();
    harness.inner().process_blocked(BlockedMarker {
        task_id: 6144,
        ..BlockedMarker::default()
    });
    assert!(harness.state().notifications[0]
        .message
        .contains("See the task for clarifying questions."));
}
