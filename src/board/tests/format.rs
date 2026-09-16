//! Row text and markers.

use std::collections::{HashMap, HashSet};

use crate::board::{format_board_item, AutoMarker, BoardCtx, BoardItem, Role, TaskSessionStatus};
use crate::types::{Color, Notification, NotificationLevel, NotificationStatus, Task};

use super::{now, role_of, task, text};

fn row_for(task: &Task, ctx: &BoardCtx<'_>) -> Vec<crate::board::Segment> {
    format_board_item(
        &BoardItem::Task {
            task,
            project_name: "Aurora",
            stage_name: "In Progress",
            sub_count: 0,
            sub_expanded: false,
        },
        ctx,
    )
}

fn ctx() -> BoardCtx<'static> {
    BoardCtx {
        now: Some(now()),
        ..BoardCtx::default()
    }
}

#[test]
fn a_plain_task_row_is_marker_name_and_id() {
    let row = row_for(&task(5944, "Fix the export"), &ctx());
    assert_eq!(text(&row), "      ○ Fix the export  #5944");
    assert_eq!(role_of(&row, "○"), Some(Role::Dim));
    assert_eq!(role_of(&row, "#5944"), Some(Role::Id));
}

#[test]
fn a_running_session_marks_the_row() {
    let sessions = HashMap::from([(5944, TaskSessionStatus::Running)]);
    let ctx = BoardCtx {
        task_sessions: Some(&sessions),
        ..ctx()
    };
    let row = row_for(&task(5944, "Fix the export"), &ctx);
    assert!(text(&row).contains("⟳"));
    assert_eq!(role_of(&row, "⟳"), Some(Role::Ok));
}

#[test]
fn a_session_the_dashboard_lost_track_of_still_marks_the_row() {
    // The daemon restarted: task_sessions is empty, but a live transcript says
    // this task is being worked right now.
    let live = HashSet::from([5944]);
    let ctx = BoardCtx {
        live_task_ids: Some(&live),
        ..ctx()
    };
    assert!(text(&row_for(&task(5944, "Fix the export"), &ctx)).contains("⟳"));
}

#[test]
fn a_finished_session_and_a_completion_marker_both_read_as_done() {
    let sessions = HashMap::from([(5944, TaskSessionStatus::Done)]);
    let from_session = BoardCtx {
        task_sessions: Some(&sessions),
        ..ctx()
    };
    assert!(text(&row_for(&task(5944, "One"), &from_session)).contains("✓"));

    let done = HashSet::from([5944]);
    let from_marker = BoardCtx {
        done_tasks: Some(&done),
        ..ctx()
    };
    assert!(text(&row_for(&task(5944, "One"), &from_marker)).contains("✓"));
}

#[test]
fn a_dependency_blocked_task_is_marked_distinctly() {
    let mut blocked = task(5238, "Per-project default templates");
    blocked.blocked_by = vec![4034];
    blocked.blocker_count = 1;
    blocked.open_blocker_count = 1;
    let row = row_for(&blocked, &ctx());
    assert!(
        text(&row).contains("⛔"),
        "a blocked task carries no marker"
    );
    assert_eq!(role_of(&row, "⛔"), Some(Role::Danger));
}

#[test]
fn a_task_whose_blockers_have_landed_is_not_marked_blocked() {
    let mut ready = task(5238, "Per-project default templates");
    ready.blocker_count = 1;
    ready.open_blocker_count = 0;
    assert!(!text(&row_for(&ready, &ctx())).contains("⛔"));
}

#[test]
fn the_needs_info_marker_wins_over_the_dependency_marker() {
    // The readiness gate stopping a task is more urgent than a dependency: it
    // means somebody has to answer a question.
    let mut blocked = task(5238, "Per-project default templates");
    blocked.open_blocker_count = 1;
    let needs_info = HashSet::from([5238]);
    let ctx = BoardCtx {
        blocked_tasks: Some(&needs_info),
        ..ctx()
    };
    let row = row_for(&blocked, &ctx);
    assert!(text(&row).contains("🚧"));
    assert!(!text(&row).contains("⛔"));
    assert!(text(&row).contains("needs-info"));
}

#[test]
fn every_badge_lands_in_its_slot() {
    let mut decorated = task(5944, "Fix the export");
    decorated.priority = Some("1".to_string());
    decorated.story_points = Some(5);
    decorated.tags = vec!["auto_implemented".to_string()];
    decorated.subtasks = vec![task(8801, "Child")];

    let archived = HashSet::from([5944]);
    let optics = HashMap::from([(5944usize as i64, 2usize)]);
    let ctx = BoardCtx {
        archived_tasks: Some(&archived),
        optics_tasks: Some(&optics),
        auto_dev: Some(|tags| {
            tags.contains(&"auto_implemented".to_string())
                .then(|| AutoMarker {
                    marker: "🤖 impl".to_string(),
                    color: Color::Cyan,
                })
        }),
        ..ctx()
    };

    let row = format_board_item(
        &BoardItem::Task {
            task: &decorated,
            project_name: "Aurora",
            stage_name: "In Progress",
            sub_count: 1,
            sub_expanded: false,
        },
        &ctx,
    );
    assert_eq!(
        text(&row),
        "    ▶ ○ ★ Fix the export  #5944 5sp 💾 🤖 impl 🔬2 ⊕1"
    );
    assert_eq!(role_of(&row, "5sp"), Some(Role::Warn));
    assert_eq!(role_of(&row, "💾"), Some(Role::Accent));
    assert_eq!(role_of(&row, "🤖 impl"), Some(Role::Accent));
    assert_eq!(role_of(&row, "🔬2"), Some(Role::Info));
    assert_eq!(role_of(&row, "⊕1"), Some(Role::Accent));
    assert_eq!(role_of(&row, "★"), Some(Role::Warn));
}

#[test]
fn an_unestimated_task_shows_no_story_points() {
    let row = row_for(&task(5944, "Fix the export"), &ctx());
    assert!(!text(&row).contains("sp"));
}

#[test]
fn a_past_deadline_is_red_and_a_future_one_is_not() {
    let mut overdue = task(5944, "Fix the export");
    overdue.deadline = Some("2026-09-01".to_string());
    let row = row_for(&overdue, &ctx());
    assert!(text(&row).ends_with(" ⚠ 09-01"));
    assert_eq!(role_of(&row, "⚠ 09-01"), Some(Role::Danger));

    let mut upcoming = task(5944, "Fix the export");
    upcoming.deadline = Some("2026-09-20".to_string());
    let row = row_for(&upcoming, &ctx());
    assert!(text(&row).ends_with(" ⏱ 09-20"));
    assert_eq!(role_of(&row, "⏱ 09-20"), Some(Role::Dim));
}

#[test]
fn a_subtask_row_is_indented_and_names_its_stage() {
    let mut child = task(8801, "Child");
    child.stage_name = "Quality Assurance".to_string();
    let row = format_board_item(
        &BoardItem::Subtask {
            task: &child,
            parent_id: 5944,
        },
        &ctx(),
    );
    assert_eq!(text(&row), "         ↳ ○ Child  #8801  Quality Assurance");
}

#[test]
fn long_names_are_truncated_to_the_column() {
    let long = task(5944, &"x".repeat(80));
    let row = row_for(&long, &ctx());
    // 42 columns for a task row, an ellipsis included.
    assert!(text(&row).contains(&"x".repeat(41)));
    assert!(!text(&row).contains(&"x".repeat(43)));
}

#[test]
fn project_and_stage_rows_count_what_they_hold() {
    let project = format_board_item(
        &BoardItem::Project {
            name: "Aurora",
            project_id: 3,
            task_count: 3,
            expanded: true,
        },
        &ctx(),
    );
    assert_eq!(text(&project), "▼ 📋 Aurora  3 tasks");

    let single = format_board_item(
        &BoardItem::Project {
            name: "Aurora",
            project_id: 3,
            task_count: 1,
            expanded: false,
        },
        &ctx(),
    );
    assert_eq!(text(&single), "▶ 📋 Aurora  1 task");

    let stage = format_board_item(
        &BoardItem::Stage {
            project_name: "Aurora",
            stage_name: "In Progress",
            stage_id: 2,
            count: 2,
            expanded: false,
        },
        &ctx(),
    );
    assert_eq!(text(&stage), "  ▶ In Progress  (2)");
    assert_eq!(role_of(&stage, "In Progress"), Some(Role::Accent));
}

#[test]
fn info_and_separator_rows() {
    assert_eq!(
        text(&format_board_item(
            &BoardItem::Info {
                name: "No tasks assigned to you."
            },
            &ctx()
        )),
        "  No tasks assigned to you."
    );
    assert!(format_board_item(&BoardItem::Separator, &ctx()).is_empty());
}

fn notification(status: NotificationStatus, level: NotificationLevel) -> Notification {
    Notification {
        id: "n1".to_string(),
        title: "Task 5944 finished".to_string(),
        message: "MR opened".to_string(),
        cwd: "/dev/aurora".to_string(),
        project: "Aurora".to_string(),
        session_id: None,
        task_id: Some(5944),
        level,
        // Half a minute before the fixed `now`, whatever timezone the suite
        // runs in.
        ts: (now().with_timezone(&chrono::Utc) - chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        status,
    }
}

#[test]
fn an_unread_notification_flashes_on_the_blink_tick() {
    let notif = notification(NotificationStatus::Unread, NotificationLevel::Success);
    let item = BoardItem::Notification { notif: &notif };

    let steady = format_board_item(&item, &ctx());
    assert!(steady.iter().all(|segment| !segment.style.invert));
    assert_eq!(role_of(&steady, "Task 5944 finished"), Some(Role::Ok));

    let flashing = format_board_item(
        &item,
        &BoardCtx {
            blink_on: true,
            ..ctx()
        },
    );
    assert!(flashing.iter().any(|segment| segment.style.invert));
}

#[test]
fn a_read_notification_never_flashes() {
    let notif = notification(NotificationStatus::Read, NotificationLevel::Warn);
    let row = format_board_item(
        &BoardItem::Notification { notif: &notif },
        &BoardCtx {
            blink_on: true,
            ..ctx()
        },
    );
    assert!(row.iter().all(|segment| !segment.style.invert));
    assert_eq!(role_of(&row, "Task 5944 finished"), Some(Role::Dim));
    assert!(text(&row).contains("30s ago"));
}

#[test]
fn the_notification_header_counts_unread_and_total() {
    let row = format_board_item(
        &BoardItem::NotificationHeader {
            unread: 2,
            total: 7,
        },
        &ctx(),
    );
    assert_eq!(text(&row), "🔔 Notifications  (2 unread / 7)");
    assert_eq!(role_of(&row, "🔔"), Some(Role::Warn));

    let quiet = format_board_item(
        &BoardItem::NotificationHeader {
            unread: 0,
            total: 7,
        },
        &ctx(),
    );
    assert_eq!(role_of(&quiet, "🔔"), Some(Role::Dim));
}
