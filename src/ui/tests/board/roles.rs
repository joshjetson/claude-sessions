//! What the role changes on the board, and the feed fixes every role gets:
//! Clear all, the menu's Resolve and Dismiss, and the daemon's feed events.

use crossterm::event::KeyCode;

use crate::types::{NotificationStatus, UserRole, QUIET_SESSIONS_ID};
use crate::ui::board;
use crate::ui::dialogs::{Dialog, NotifMenu, TaskAction, TaskMenu};
use crate::ui::state::Action;

use super::fixtures::*;

fn labels(menu: &TaskMenu) -> Vec<String> {
    menu.entries
        .iter()
        .map(|(label, _)| label.clone())
        .collect()
}

fn select(state: &mut crate::ui::state::AppState, key: &str) {
    let snapshot = board::snapshot(state);
    let index = snapshot
        .keys
        .iter()
        .position(|candidate| candidate == key)
        .unwrap_or_else(|| panic!("no row {key}: {:?}", snapshot.keys));
    state.board_sel.set(&snapshot.keys, index);
}

#[test]
fn the_qa_menu_has_no_dev_launches_and_keeps_the_qa_and_shared_ones() {
    let (_dir, mut state) = board_state();
    state.role = UserRole::Qa;
    state.board.archived_tasks.insert(5238);
    let menu = TaskMenu::build(&task(5238, "x"), &state);
    let labels = labels(&menu);
    for gone in ["Start task", "Add context & start", "Resume for revision"] {
        assert!(
            !labels.iter().any(|label| label.contains(gone)),
            "QA still offers {gone}: {labels:?}"
        );
    }
    for kept in [
        "dry run",
        "Pre-work brief",
        "Resume conversation (no prompt)",
        "Move to stage",
        "Open in browser",
    ] {
        assert!(
            labels.iter().any(|label| label.contains(kept)),
            "QA lost {kept}: {labels:?}"
        );
    }
    assert!(menu
        .entries
        .iter()
        .any(|(_, action)| matches!(action, TaskAction::Start(crate::ui::board::LaunchKind::Qa))));
}

#[test]
fn dev_and_pm_menus_are_unchanged() {
    for role in [UserRole::Dev, UserRole::Pm] {
        let (_dir, mut state) = board_state();
        state.role = role;
        state.board.archived_tasks.insert(5238);
        let labels = labels(&TaskMenu::build(&task(5238, "x"), &state));
        for needle in [
            "Start task",
            "Add context & start",
            "Resume for revision (prior context)",
            "Resume for revision + add context",
        ] {
            assert!(
                labels.iter().any(|label| label.contains(needle)),
                "{role:?} lost {needle}"
            );
        }
    }
}

#[test]
fn the_qa_role_s_v_and_c_keys_launch_nothing_and_say_why() {
    let (_dir, mut state) = board_state();
    state.role = UserRole::Qa;
    with_tasks(&mut state, vec![task(5238, "x")]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    select(&mut state, "bt:5238");
    for key in ['s', 'v', 'C'] {
        state.flash = None;
        super::press(&mut state, KeyCode::Char(key));
        assert!(state.dialog.is_none(), "{key} opened a dialog");
        assert!(
            !actions(&mut state)
                .iter()
                .any(|a| matches!(a, Action::Launch(_) | Action::Resume(_))),
            "{key} launched"
        );
        assert!(
            state.flash.clone().unwrap_or_default().contains("QA"),
            "{key} said nothing"
        );
    }
    // A shared key still works.
    super::press(&mut state, KeyCode::Char('m'));
    assert!(matches!(state.dialog, Some(Dialog::StagePicker(_))));
}

#[test]
fn the_dev_role_s_key_still_starts() {
    let (_dir, mut state) = board_state();
    with_tasks(&mut state, vec![task(5238, "x")]);
    state.board.expanded.insert(crate::board::stage_key(
        "NoSuchProject-ForTests",
        "Approved to Start",
    ));
    select(&mut state, "bt:5238");
    super::press(&mut state, KeyCode::Char('s'));
    // No folder is mapped, so the start flow asks for one: it ran.
    assert!(
        state.dialog.is_some() || !actions(&mut state).is_empty(),
        "s did nothing for the dev role"
    );
}

#[test]
fn the_hints_follow_the_role() {
    let dev = crate::ui::app::board_hints(UserRole::Dev).join("\n");
    assert!(dev.contains("s start"));
    let qa = crate::ui::app::board_hints(UserRole::Qa).join("\n");
    assert!(!qa.contains("s start") && !qa.contains("v revise"));
    assert!(qa.contains("QA"));
    assert!(qa.contains("clear all"));
}

// --- Clear all ---------------------------------------------------------------

#[test]
fn x_on_the_feed_header_clears_every_notification() {
    let (_dir, mut state) = board_state();
    for id in ["n1", "n2", "n3"] {
        state.push_notification(notification(id, None));
    }
    state.take_actions();
    select(&mut state, "nhdr");
    super::press(&mut state, KeyCode::Char('x'));

    assert!(state
        .notifications
        .iter()
        .all(|n| n.status == NotificationStatus::Resolved));
    assert!(actions(&mut state).contains(&Action::ClearNotifications));
    let snapshot = board::snapshot(&state);
    assert!(
        !snapshot.keys.iter().any(|key| key.starts_with("n:")),
        "cleared rows are still drawn"
    );
}

#[test]
fn x_on_a_row_dismisses_only_that_row() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.push_notification(notification("n2", None));
    state.take_actions();
    select(&mut state, "n:n1");
    super::press(&mut state, KeyCode::Char('x'));
    assert!(state.notification("n1").is_none());
    assert!(state.notification("n2").is_some());
    assert!(actions(&mut state).contains(&Action::Notifications {
        ids: vec!["n1".to_string()],
        status: None,
    }));
}

// --- the menu's Resolve and Dismiss -----------------------------------------

fn pick(state: &mut crate::ui::state::AppState, label_index: usize) {
    for _ in 0..label_index {
        super::press(state, KeyCode::Down);
    }
    super::press(state, KeyCode::Enter);
}

#[test]
fn the_menus_resolve_changes_the_row_now_and_tells_the_owner() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.take_actions();
    state.dialog = Some(Dialog::NotifMenu(NotifMenu::new(
        notification("n1", None),
        None,
    )));
    // No live session: Resolve, Dismiss, Cancel.
    pick(&mut state, 0);
    assert_eq!(
        state.notification("n1").map(|n| n.status),
        Some(NotificationStatus::Resolved)
    );
    assert!(actions(&mut state).contains(&Action::Notifications {
        ids: vec!["n1".to_string()],
        status: Some(NotificationStatus::Resolved),
    }));
}

#[test]
fn the_menus_dismiss_removes_the_row_now() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("n1", None));
    state.take_actions();
    state.dialog = Some(Dialog::NotifMenu(NotifMenu::new(
        notification("n1", None),
        None,
    )));
    pick(&mut state, 1);
    assert!(state.notification("n1").is_none());
    assert!(actions(&mut state).contains(&Action::Notifications {
        ids: vec!["n1".to_string()],
        status: None,
    }));
}

// --- the daemon's feed events -----------------------------------------------

#[test]
fn a_snapshot_replaces_the_feed_silently() {
    let (_dir, mut state) = board_state();
    state.push_notification(notification("stale", None));
    state.take_actions();
    state.replace_notifications(vec![notification("a", None), notification("b", None)]);
    assert!(state.notification("stale").is_none());
    assert_eq!(state.notifications.len(), 2);
    assert!(
        !actions(&mut state)
            .iter()
            .any(|a| matches!(a, Action::Sound(_))),
        "the backlog rang"
    );
}

#[test]
fn a_change_from_the_daemon_applies_by_id() {
    let (_dir, mut state) = board_state();
    state.replace_notifications(vec![notification("a", None), notification("b", None)]);
    state.apply_notifications_changed(
        &["a".to_string()],
        Some(NotificationStatus::Resolved),
        false,
    );
    assert_eq!(
        state.notification("a").map(|n| n.status),
        Some(NotificationStatus::Resolved)
    );
    state.apply_notifications_changed(&["b".to_string()], None, true);
    assert!(state.notification("b").is_none());
}

/// The quiet row: raised once with a sound, then updated in place in silence,
/// pinned to the top of the feed.
#[test]
fn the_quiet_row_rings_once_updates_silently_and_is_pinned_first() {
    let (_dir, mut state) = board_state();
    let quiet = |title: &str| crate::types::Notification {
        id: QUIET_SESSIONS_ID.to_string(),
        title: title.to_string(),
        ..notification(QUIET_SESSIONS_ID, None)
    };
    state.push_notification(quiet("⏳ 2 sessions quiet"));
    state.push_notification(notification("newer", None));
    let sounds = |state: &mut crate::ui::state::AppState| {
        actions(state)
            .iter()
            .filter(|a| matches!(a, Action::Sound(_)))
            .count()
    };
    assert_eq!(sounds(&mut state), 2);

    for n in 3..10 {
        state.upsert_notification(quiet(&format!("⏳ {n} sessions quiet")));
    }
    assert_eq!(sounds(&mut state), 0, "an update rang");
    assert_eq!(
        state
            .notifications
            .iter()
            .filter(|n| n.id == QUIET_SESSIONS_ID)
            .count(),
        1
    );
    let snapshot = board::snapshot(&state);
    assert_eq!(
        snapshot.keys.get(1).map(String::as_str),
        Some(&*format!("n:{QUIET_SESSIONS_ID}")),
        "the quiet row is not pinned first: {:?}",
        snapshot.keys
    );

    // Re-raised under the same id: one row, not two.
    state.push_notification(quiet("⏳ again"));
    assert_eq!(
        state
            .notifications
            .iter()
            .filter(|n| n.id == QUIET_SESSIONS_ID)
            .count(),
        1
    );
}

// --- the badges in the detail pane -------------------------------------------

#[test]
fn the_detail_pane_names_the_odoo_state_of_a_task_in_qa() {
    for (state_value, expected) in [
        ("03_approved", "✅ Marked Complete in Odoo"),
        ("02_changes_requested", "🔁 Changes Requested in Odoo"),
    ] {
        let (_dir, mut state) = board_state();
        let in_qa = crate::types::Task {
            stage_name: "QA".to_string(),
            state: Some(state_value.to_string()),
            ..task(5238, "x")
        };
        with_tasks(&mut state, vec![in_qa.clone()]);
        let detail = crate::ui::board::detail::state_of(&state, 5238);
        let rows = crate::ui::board::detail::task_header(&in_qa, detail);
        let text: Vec<String> = rows
            .iter()
            .map(|row| crate::board::plain_text(row))
            .collect();
        assert!(text.iter().any(|line| line == expected), "{text:?}");
    }
}

// --- with no daemon ----------------------------------------------------------

/// An embedded feed runs no engine, so the worker writes the change to SQLite.
/// A daemon started later restores its feed from there and must not bring back
/// what was cleared or dismissed.
#[test]
fn with_no_daemon_the_worker_writes_changes_to_sqlite() {
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    let harness = super::launch::harness();
    let db = crate::db::Db::open(&harness.services.paths);
    for id in ["a", "b", "c"] {
        db.put_notification(&notification(id, None));
    }
    let driver: Arc<dyn crate::term::TerminalDriver> = harness.driver.clone();
    let (tx, _rx) = channel();
    let run = |action: Action| {
        crate::ui::actions::run_for_test(
            action,
            &driver,
            crate::term::SpawnPolicy::Refuse,
            &harness.services,
            &tx,
        )
    };

    run(Action::Notifications {
        ids: vec!["a".to_string()],
        status: None,
    });
    run(Action::Notifications {
        ids: vec!["b".to_string()],
        status: Some(NotificationStatus::Read),
    });
    let stored = db.recent_notifications(10);
    assert!(
        !stored.iter().any(|n| n.id == "a"),
        "dismissed row survived"
    );
    assert_eq!(
        stored.iter().find(|n| n.id == "b").map(|n| n.status),
        Some(NotificationStatus::Read)
    );

    run(Action::ClearNotifications);
    assert!(db
        .recent_notifications(10)
        .iter()
        .all(|n| n.status == NotificationStatus::Resolved));
}
