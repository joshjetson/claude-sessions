//! Errors reach the screen as red rows in the Notifications feed and nowhere
//! else. These pin how a stream of them is folded, so a repeating failure
//! reads as one row with a count rather than a wall of copies that each ring.

use std::time::{Duration, Instant};

use crate::errorlog::ErrorReport;
use crate::types::{NotificationLevel, NotificationStatus};
use crate::ui::state::{Action, AppState, ERROR_COALESCE_WINDOW, LOCAL_ERROR_PREFIX};

use super::temp_state;

fn report(source: &str, message: &str) -> ErrorReport {
    ErrorReport {
        source: source.to_string(),
        message: message.to_string(),
    }
}

fn sounds(state: &mut AppState) -> usize {
    state
        .take_actions()
        .iter()
        .filter(|action| matches!(action, Action::Sound(NotificationLevel::Error)))
        .count()
}

#[test]
fn an_error_becomes_one_unread_red_row_that_rings() {
    let (_dir, mut state) = temp_state();
    state.push_error_at(report("kill", "No such process"), Instant::now());

    assert_eq!(state.notifications.len(), 1);
    let row = &state.notifications[0];
    assert!(row.id.starts_with(LOCAL_ERROR_PREFIX), "{}", row.id);
    assert_eq!(row.level, NotificationLevel::Error);
    assert_eq!(row.status, NotificationStatus::Unread);
    assert_eq!(row.title, "Error: kill");
    assert_eq!(row.message, "No such process");
    assert_eq!(sounds(&mut state), 1);
}

#[test]
fn a_repeat_inside_the_window_bumps_the_existing_row_silently() {
    let (_dir, mut state) = temp_state();
    let start = Instant::now();
    state.push_error_at(report("kill", "No such process"), start);
    assert_eq!(sounds(&mut state), 1);
    let id = state.notifications[0].id.clone();
    // Read in the meantime: a repeat must bring it back to attention.
    state.set_notification_status(&id, NotificationStatus::Read);

    state.push_error_at(
        report("kill", "No such process"),
        start + Duration::from_secs(5),
    );
    state.push_error_at(
        report("kill", "No such process"),
        start + Duration::from_secs(10),
    );

    assert_eq!(state.notifications.len(), 1, "a repeat stacked a new row");
    let row = &state.notifications[0];
    assert_eq!(row.id, id);
    assert_eq!(row.title, "Error: kill (×3)");
    assert_eq!(row.status, NotificationStatus::Unread);
    assert_eq!(sounds(&mut state), 0, "a repeat replayed the sound");
}

#[test]
fn a_repeat_moves_its_row_back_to_the_top() {
    let (_dir, mut state) = temp_state();
    let start = Instant::now();
    state.push_error_at(report("kill", "one"), start);
    state.push_error_at(report("panic", "two"), start + Duration::from_secs(1));
    assert_eq!(state.notifications[0].message, "two");

    state.push_error_at(report("kill", "one"), start + Duration::from_secs(2));
    assert_eq!(state.notifications.len(), 2);
    assert_eq!(state.notifications[0].message, "one");
    assert_eq!(state.notifications[0].title, "Error: kill (×2)");
}

#[test]
fn different_errors_do_not_fold_together() {
    let (_dir, mut state) = temp_state();
    let now = Instant::now();
    state.push_error_at(report("kill", "No such process"), now);
    state.push_error_at(report("kill", "Operation not permitted"), now);
    state.push_error_at(report("panic", "No such process"), now);
    assert_eq!(state.notifications.len(), 3);
    assert_eq!(sounds(&mut state), 3);
}

#[test]
fn a_repeat_after_the_window_is_a_new_row() {
    let (_dir, mut state) = temp_state();
    let start = Instant::now();
    state.push_error_at(report("kill", "No such process"), start);
    let later = start + ERROR_COALESCE_WINDOW + Duration::from_secs(1);
    state.push_error_at(report("kill", "No such process"), later);

    assert_eq!(state.notifications.len(), 2);
    assert_ne!(state.notifications[0].id, state.notifications[1].id);
    assert_eq!(state.notifications[0].title, "Error: kill");
    assert_eq!(sounds(&mut state), 2);
}

#[test]
fn a_repeat_after_a_dismiss_is_a_new_row() {
    let (_dir, mut state) = temp_state();
    let start = Instant::now();
    state.push_error_at(report("kill", "No such process"), start);
    let id = state.notifications[0].id.clone();
    state.dismiss_notification(&id);

    state.push_error_at(
        report("kill", "No such process"),
        start + Duration::from_secs(1),
    );
    assert_eq!(state.notifications.len(), 1);
    assert_ne!(state.notifications[0].id, id);
    assert_eq!(state.notifications[0].title, "Error: kill");
}
