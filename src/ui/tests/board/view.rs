//! The board tab's rows, cursor, detail pane and key map.

use crossterm::event::KeyCode;

use crate::daemon::BoardFilter;
use crate::ui::board::detail::html_to_text;
use crate::ui::board::{self, BoardRow, BoardUpdate};
use crate::ui::state::Action;

use super::fixtures::*;

#[test]
fn the_feed_sits_above_the_tree_and_resolved_notifications_are_gone() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "Report templates")]);
    state.push_notification(notification("n1", None));
    let mut resolved = notification("n2", None);
    resolved.status = crate::types::NotificationStatus::Resolved;
    state.push_notification(resolved);
    state.take_actions();

    let snapshot = board::snapshot(&state);
    assert_eq!(snapshot.keys.first().map(String::as_str), Some("nhdr"));
    assert!(snapshot.keys.contains(&"n:n1".to_string()));
    assert!(
        !snapshot.keys.contains(&"n:n2".to_string()),
        "a resolved notification is still in the feed"
    );
    assert!(snapshot.keys.contains(&"nsep".to_string()));
}

#[test]
fn a_first_load_opens_every_project() {
    // Otherwise the tab is a list of collapsed headers the first time you
    // press Tab.
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    assert!(state
        .board
        .expanded
        .contains(&crate::board::project_key("NoSuchProject-ForTests")));
}

#[test]
fn a_refresh_leaves_the_expansion_set_alone() {
    // A board that re-collapsed itself every 45 seconds would be unusable.
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    with_tasks(&mut state, vec![task(5238, "x"), task(5239, "y")]);
    assert!(state.board.expanded.contains(&crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start"
    )));
}

#[test]
fn changing_the_filter_drops_the_board_it_was_showing() {
    // The old board is the OTHER filter's answer; leaving it up displays
    // exactly the tasks that were just filtered out.
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    super::press(&mut state, KeyCode::Char('f'));
    assert_eq!(state.board.filter, BoardFilter::All);
    assert!(state.board.board.is_none());
    assert!(state.board.expanded.is_empty());
    assert!(state
        .take_actions()
        .iter()
        .any(|a| matches!(a, Action::RefreshBoard(_))));
}

#[test]
fn the_filter_reaches_the_query() {
    let (_dir, mut state) = board_state();
    assert!(board::fetch_options(&state).mine_only);
    state.board.set_filter(BoardFilter::All);
    assert!(!board::fetch_options(&state).mine_only);
}

#[test]
fn a_board_with_no_credentials_says_why_rather_than_looking_empty() {
    let (_dir, state) = board_state();
    let items = board::view::build_items(&state.board, &state.notifications);
    let text: String = items
        .iter()
        .map(|item| {
            crate::board::plain_text(&crate::board::format_board_item(item, &Default::default()))
        })
        .collect();
    assert!(text.contains("No Odoo credentials"), "{text}");
}

#[test]
fn a_board_still_being_fetched_says_so_rather_than_showing_an_empty_pane() {
    // What a dashboard sees while a fresh daemon is warming its board: the
    // snapshot carries `boardLoading` and no board, and the pane has to say
    // which of the two empty states this is.
    let (_dir, mut state) = board_state();
    state.apply_board(BoardUpdate::loading(BoardFilter::Mine));
    assert!(state.board.loading);
    let items = board::view::build_items(&state.board, &state.notifications);
    let text: String = items
        .iter()
        .map(|item| {
            crate::board::plain_text(&crate::board::format_board_item(item, &Default::default()))
        })
        .collect();
    assert!(text.contains("Loading tasks from Odoo"), "{text}");
    assert!(!text.contains("No Odoo credentials"), "{text}");
}

#[test]
fn a_failed_fetch_keeps_the_previous_board_and_says_what_went_wrong() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "Report templates")]);
    state.apply_board(BoardUpdate::failed(BoardFilter::Mine, "Odoo said no"));
    assert!(state.board.board.is_some(), "the board was thrown away");
    let items = board::view::build_items(&state.board, &state.notifications);
    let text: String = items
        .iter()
        .map(|item| {
            crate::board::plain_text(&crate::board::format_board_item(item, &Default::default()))
        })
        .collect();
    assert!(text.contains("Odoo said no"), "{text}");
    assert!(text.contains("Press r to retry"), "{text}");
}

#[test]
fn right_on_a_task_with_subtasks_opens_them_before_showing_the_detail() {
    let (_dir, mut state) = board_state();
    let mut parent = task(5238, "Parent");
    parent.subtasks = vec![task(5239, "Child")];
    with_tasks(&mut state, vec![parent]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    // Put the cursor on the task row.
    let snapshot = board::snapshot(&state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key == "bt:5238")
        .expect("the task row");
    state.board_sel.set(&snapshot.keys, index);

    super::press(&mut state, KeyCode::Right);
    assert!(state
        .board
        .expanded
        .contains(&crate::board::subtask_key(5238)));
    assert!(
        state.board.detail.is_none(),
        "it skipped straight to detail"
    );

    super::press(&mut state, KeyCode::Right);
    assert!(
        state.board.detail.is_some(),
        "a second press showed nothing"
    );
    assert!(state
        .take_actions()
        .iter()
        .any(|a| matches!(a, Action::FetchTaskDescription { task_id: 5238 })));
}

#[test]
fn left_on_a_subtask_collapses_the_parent_that_holds_it() {
    let (_dir, mut state) = board_state();
    let mut parent = task(5238, "Parent");
    parent.subtasks = vec![task(5239, "Child")];
    with_tasks(&mut state, vec![parent]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    state.board.expanded.insert(crate::board::subtask_key(5238));
    let snapshot = board::snapshot(&state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key == "st:5239")
        .expect("the subtask row");
    state.board_sel.set(&snapshot.keys, index);

    super::press(&mut state, KeyCode::Left);
    assert!(!state
        .board
        .expanded
        .contains(&crate::board::subtask_key(5238)));
}

#[test]
fn the_detail_pane_shows_the_gates_questions_for_a_task_that_needs_info() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "Report templates")]);
    state
        .board
        .blocked_tasks
        .insert(5238, vec!["which radiologist wins?".to_string()]);
    let pane = crate::ui::board::detail::loading(
        &task(5238, "Report templates"),
        crate::ui::board::detail::state_of(&state, 5238),
    );
    let text: String = pane
        .rows
        .iter()
        .map(|row| format!("{}\n", crate::board::plain_text(row)))
        .collect();
    assert!(text.contains("Needs info"), "{text}");
    assert!(text.contains("which radiologist wins?"), "{text}");
    assert!(text.contains("press s to re-run"), "{text}");
}

#[test]
fn the_detail_pane_says_when_a_transcript_is_archived() {
    let (_dir, mut state) = board_state();
    state.board.archived_tasks.insert(5238);
    let pane = crate::ui::board::detail::loading(
        &task(5238, "x"),
        crate::ui::board::detail::state_of(&state, 5238),
    );
    let text: String = pane
        .rows
        .iter()
        .map(|row| crate::board::plain_text(row))
        .collect();
    assert!(text.contains("transcript archived"), "{text}");
    assert_eq!(pane.task_id, Some(5238));
}

#[test]
fn a_description_for_another_task_never_lands_in_the_open_pane() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    state.board.detail = Some(crate::ui::board::detail::loading(
        &task(5238, "x"),
        Default::default(),
    ));
    crate::ui::board::detail::apply_description(&mut state, 9999, None);
    let text: String = state
        .board
        .detail
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|row| crate::board::plain_text(row))
        .collect();
    assert!(
        text.contains("loading description"),
        "the stale answer landed"
    );
}

#[test]
fn odoo_html_becomes_something_a_terminal_can_show() {
    assert_eq!(html_to_text("<p>One</p><p>Two</p>"), "One\nTwo");
    assert_eq!(html_to_text("<ul><li>a</li><li>b</li></ul>"), "• a\n• b");
    assert_eq!(html_to_text("a<br/>b"), "a\nb");
    assert_eq!(html_to_text("&lt;tag&gt; &amp; more"), "<tag> & more");
    // An escaped entity in the source does not become a real one.
    assert_eq!(html_to_text("&amp;lt;"), "&lt;");
    // Odoo's editor emits an empty <p> per blank line; they must not stack.
    assert_eq!(html_to_text("<p>a</p><p></p><p></p><p>b</p>"), "a\n\nb");
}

#[test]
fn dismissing_a_notification_removes_it_and_tells_the_owner() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.take_actions();
    let snapshot = board::snapshot(&state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key == "n:n1")
        .expect("the notification row");
    state.board_sel.set(&snapshot.keys, index);

    super::press(&mut state, KeyCode::Char('x'));
    assert!(state.notifications.is_empty());
    let queued = state.take_actions();
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::Notifications { ids, status: None } if ids == &vec!["n1".to_string()]
        )),
        "{queued:?}"
    );
}

#[test]
fn opening_a_notification_marks_it_read() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.take_actions();
    let snapshot = board::snapshot(&state);
    let index = snapshot.keys.iter().position(|key| key == "n:n1").unwrap();
    state.board_sel.set(&snapshot.keys, index);

    super::press(&mut state, KeyCode::Enter);
    assert_eq!(
        state.notification("n1").map(|n| n.status),
        Some(crate::types::NotificationStatus::Read)
    );
    assert_eq!(
        state.dialog.as_ref().map(crate::ui::dialogs::Dialog::name),
        Some("notifMenu")
    );
}

#[test]
fn a_notification_rings_once_when_it_arrives() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    let queued = state.take_actions();
    assert_eq!(
        queued
            .iter()
            .filter(|a| matches!(a, Action::Sound(_)))
            .count(),
        1,
        "{queued:?}"
    );
}

#[test]
fn ssh_and_the_browser_work_from_any_row_in_a_projects_block() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    // The project header row.
    let snapshot = board::snapshot(&state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("bp:"))
        .unwrap();
    state.board_sel.set(&snapshot.keys, index);
    super::press(&mut state, KeyCode::Char('S'));
    let queued = state.take_actions();
    assert!(
        queued.iter().any(|action| matches!(
            action,
            Action::Ssh { project } if project == "NoSuchProject-ForTests"
        )),
        "{queued:?}"
    );
}

#[test]
fn m_opens_the_merge_request_browser_and_asks_for_the_list() {
    let (_dir, mut state) = board_state();
    super::press(&mut state, KeyCode::Char('M'));
    assert_eq!(
        state.dialog.as_ref().map(crate::ui::dialogs::Dialog::name),
        Some("openMRs")
    );
    assert!(state
        .take_actions()
        .iter()
        .any(|action| matches!(action, Action::FetchOpenMrs)));
}

#[test]
fn a_row_key_on_a_row_that_is_not_a_task_does_nothing() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.take_actions();
    let snapshot = board::snapshot(&state);
    let index = snapshot.keys.iter().position(|key| key == "n:n1").unwrap();
    state.board_sel.set(&snapshot.keys, index);
    for key in ['s', 'v', 'C', 'm', 'g', 'G', 'o'] {
        super::press(&mut state, KeyCode::Char(key));
        assert!(
            state.take_actions().is_empty(),
            "{key} acted on a notification row"
        );
        assert!(state.dialog.is_none(), "{key} opened a dialog");
    }
}

#[test]
fn a_row_is_identified_by_what_it_is_about() {
    let row = BoardRow::Subtask {
        task: Box::new(task(5239, "child")),
        parent_id: 5238,
    };
    assert_eq!(row.task().map(|t| t.id), Some(5239));
    assert_eq!(row.project_name(), Some("NoSuchProject-ForTests"));
    assert_eq!(row.expand_key(), Some(crate::board::subtask_key(5238)));
}

#[test]
fn the_whole_tab_paints_the_feed_the_tree_and_the_detail_pane() {
    // End to end through the real draw: board rows are `Segment`s, and this is
    // the only thing that proves the segment-to-span mapping is wired up.
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "Report templates")]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    state.push_notification(notification("n1", None));
    state.take_actions();

    let painted = crate::ui::tests::text(&crate::ui::tests::render(120, 24, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(painted.contains("Tasks Board · mine"), "{painted}");
    assert!(painted.contains("Notifications"), "{painted}");
    assert!(painted.contains("needs a decision"), "{painted}");
    assert!(painted.contains("NoSuchProject-ForTests"), "{painted}");
    assert!(painted.contains("Approved to Start"), "{painted}");
    assert!(painted.contains("Report templates"), "{painted}");
    assert!(painted.contains("#5238"), "{painted}");
    // The status bar advertises only what the tab can actually do.
    assert!(painted.contains("s start"), "{painted}");
}

#[test]
fn the_detail_pane_is_labelled_by_what_it_is_showing() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "Report templates")]);
    state.push_notification(notification("n1", None));
    state.take_actions();
    let snapshot = board::snapshot(&state);
    let index = snapshot.keys.iter().position(|key| key == "n:n1").unwrap();
    state.board_sel.set(&snapshot.keys, index);
    super::press(&mut state, KeyCode::Right);

    let painted = crate::ui::tests::text(&crate::ui::tests::render(120, 24, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(painted.contains("Notification"), "{painted}");
    assert!(painted.contains("which behaviour should win?"), "{painted}");
}

#[test]
fn only_the_visible_window_of_a_long_board_is_formatted() {
    // Brief §10 mandate #7 — the pane cannot paint more rows than it holds.
    let (_dir, mut state) = board_state();
    let tasks: Vec<crate::types::Task> = (0..400).map(|i| task(5000 + i, "row")).collect();
    with_tasks(&mut state, tasks);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    let view = board::window(&state, 0, 10, 133);
    assert!(view.total > 400);
    assert_eq!(view.lines.len(), 10);
}
