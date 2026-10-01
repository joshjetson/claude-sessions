//! The key table: keys by role, remapped keys, the status bar, the `?` help
//! and the settings Keys page.

use std::collections::{BTreeMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use super::dialogs::{press as press_dialog, temp_config_paths};
use crate::daemon::BoardFilter;
use crate::types::{SessionStatus, UserRole};
use crate::ui::dialogs::settings::Page;
use crate::ui::dialogs::{Dialog, SettingsDialog};
use crate::ui::keymap::{fit_hints, hints, Key, Keymap, Scope, BINDINGS};
use crate::ui::keys::handle_key;
use crate::ui::state::{Action, AppState, Pane, View};
use crate::ui::tests::{session, sessions_state};

const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 120,
    height: 40,
};

fn press(state: &mut AppState, ch: char) {
    handle_key(
        state,
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        AREA,
    );
}

fn board_as(role: UserRole) -> (tempfile::TempDir, AppState) {
    let (dir, mut state) = sessions_state();
    state.view = View::Board;
    state.role = role;
    (dir, state)
}

fn bar_keys(state: &AppState) -> Vec<String> {
    let keymap = Keymap::new(state.role, state.config.key_overrides());
    hints(&keymap, state.view, state.focus)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

fn remap(id: &str, ch: char) -> BTreeMap<String, String> {
    [(id.to_string(), ch.to_string())].into_iter().collect()
}

// --- the table -------------------------------------------------------------

#[test]
fn every_binding_has_its_own_id_and_its_own_key_in_its_scope() {
    let mut ids = HashSet::new();
    let mut keys = HashSet::new();
    for binding in BINDINGS {
        assert!(ids.insert(binding.id), "{} twice", binding.id);
        if let Key::Char(ch) = binding.key {
            assert!(
                keys.insert((format!("{:?}", binding.scope), ch)),
                "`{ch}` twice in {:?}",
                binding.scope
            );
        }
        assert!(!binding.help.is_empty(), "{} has no help", binding.id);
    }
}

#[test]
fn a_bad_remap_in_the_file_is_ignored() {
    let overrides: BTreeMap<String, String> = [
        ("board.nothing", "w"),
        ("board.filter", "ww"),
        ("board.stage", "j"),
        ("global.quit", "x"),
    ]
    .into_iter()
    .map(|(id, key)| (id.to_string(), key.to_string()))
    .collect();
    let keymap = Keymap::new(UserRole::Dev, &overrides);
    for id in ["board.filter", "board.stage", "global.quit"] {
        let binding = Keymap::binding(id).unwrap();
        assert_eq!(keymap.key_of(binding), binding.key, "{id} moved");
    }
}

// --- roles -----------------------------------------------------------------

#[test]
fn the_qa_board_offers_no_development_keys() {
    let (_dir, state) = board_as(UserRole::Qa);
    let keys = bar_keys(&state);
    for gone in ["s", "v", "C", "P", "S", "M", "D"] {
        assert!(
            !keys.contains(&gone.to_string()),
            "QA bar has {gone}: {keys:?}"
        );
    }
    for kept in ["R", "]", "a", "A", "m"] {
        assert!(
            keys.contains(&kept.to_string()),
            "QA bar lost {kept}: {keys:?}"
        );
    }
}

#[test]
fn the_dev_board_offers_no_qa_run_keys_and_pm_keeps_every_key() {
    let (_dir, state) = board_as(UserRole::Dev);
    let keys = bar_keys(&state);
    for gone in ["R", "A", "]"] {
        assert!(
            !keys.contains(&gone.to_string()),
            "dev bar has {gone}: {keys:?}"
        );
    }
    assert!(keys.contains(&"P".to_string()), "{keys:?}");

    let (_dir, state) = board_as(UserRole::Pm);
    let keys = bar_keys(&state);
    for kept in ["R", "A", "]", "s", "P"] {
        assert!(
            keys.contains(&kept.to_string()),
            "PM bar lost {kept}: {keys:?}"
        );
    }
}

#[test]
fn a_key_the_role_is_not_offered_does_nothing_and_says_why() {
    let (_dir, mut state) = board_as(UserRole::Qa);
    press(&mut state, 'P');
    assert!(state.dialog.is_none(), "P opened {:?}", state.dialog);
    let said = state.flash.clone().unwrap_or_default();
    assert!(said.contains("QA role does not offer `P`"), "{said}");

    let (_dir, mut state) = board_as(UserRole::Dev);
    press(&mut state, 'R');
    assert!(state.board.runs.is_empty(), "R made a run for a developer");
    assert!(state.flash.clone().unwrap_or_default().contains("DEV role"));
}

#[test]
fn the_qa_role_s_key_points_to_the_qa_launch() {
    let (_dir, mut state) = board_as(UserRole::Qa);
    press(&mut state, 's');
    let said = state.flash.clone().unwrap_or_default();
    assert!(said.contains("pick QA to start a pass"), "{said}");
}

// --- remapping ---------------------------------------------------------------

#[test]
fn a_remapped_key_does_the_job_and_the_old_key_stops() {
    let (_dir, mut state) = board_as(UserRole::Dev);
    state.config.set_key("board.filter", Some('w')).unwrap();
    assert_eq!(state.board.filter, BoardFilter::Mine);

    press(&mut state, 'f');
    assert_eq!(
        state.board.filter,
        BoardFilter::Mine,
        "the old key still works"
    );
    press(&mut state, 'w');
    assert_eq!(state.board.filter, BoardFilter::All);
    assert!(state
        .take_actions()
        .iter()
        .any(|action| matches!(action, Action::RefreshBoard(_))));
}

#[test]
fn a_rename_moved_off_r_leaves_r_as_refresh_on_a_session_row() {
    let (_dir, mut state) = sessions_state();
    state.apply_sessions(
        crate::ui::feed::group_sessions(vec![session(
            "aaa",
            "/Users/x/dev/alpha",
            SessionStatus::Idle,
        )]),
        true,
    );
    state.config.set_key("sessions.rename", Some('e')).unwrap();
    handle_key(&mut state, KeyEvent::from(KeyCode::Down), AREA);
    state.take_actions();

    press(&mut state, 'r');
    assert!(state.dialog.is_none(), "r still renames");
    assert!(state.take_actions().contains(&Action::Refresh));

    press(&mut state, 'e');
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("rename"));
}

#[test]
fn a_clash_is_refused_and_a_default_can_always_come_back() {
    let none = BTreeMap::new();
    let keymap = Keymap::new(UserRole::Dev, &none);
    // `r` is refresh everywhere, so the board cannot have it.
    let clash = keymap.refuse("board.stage", 'r').unwrap();
    assert!(clash.contains("already refresh"), "{clash}");
    assert!(
        keymap.refuse("board.stage", 'j').is_some(),
        "j is for moving"
    );
    assert!(keymap.refuse("global.quit", 'x').is_some(), "q is fixed");
    // The board and the deploy tab never answer at once.
    assert!(keymap.refuse("board.stage", 'X').is_none());

    // Rename moved off `r` can come back, though refresh has `r` too.
    let moved = Keymap::new(UserRole::Dev, &remap("sessions.rename", 'e'));
    assert!(moved.refuse("sessions.rename", 'r').is_none());
}

// --- the status bar ------------------------------------------------------------

#[test]
fn the_bar_always_ends_with_help_and_quit_however_narrow() {
    let none = BTreeMap::new();
    let keymap = Keymap::new(UserRole::Qa, &none);
    let all = hints(&keymap, View::Board, Pane::Tree);
    let narrow = fit_hints(&keymap, all.clone(), 30);
    let keys: Vec<&str> = narrow.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys.last(), Some(&"q"));
    assert!(keys.contains(&"?"));
    assert!(narrow.len() < all.len());
    // The first hint that fits is the most useful one.
    assert_eq!(narrow.first().map(|(key, _)| key.as_str()), Some("Enter"));
}

#[test]
fn the_bar_shows_a_remapped_key() {
    let (_dir, mut state) = board_as(UserRole::Dev);
    state.config.set_key("board.stage", Some('w')).unwrap();
    let keys = bar_keys(&state);
    assert!(keys.contains(&"w".to_string()), "{keys:?}");
    assert!(!keys.contains(&"m".to_string()), "{keys:?}");
}

// --- help ------------------------------------------------------------------------

#[test]
fn question_mark_lists_this_role_s_keys_for_the_view() {
    let (_dir, mut state) = board_as(UserRole::Qa);
    state.config.set_key("board.stage", Some('w')).unwrap();
    press(&mut state, '?');
    let Some(Dialog::Help(help)) = &state.dialog else {
        panic!("? opened {:?}", state.dialog.as_ref().map(|d| d.name()));
    };
    let text = format!("{:?}", help.lines);
    assert!(text.contains("Watch the stage as a QA run"), "{text}");
    assert!(!text.contains("pipeline"), "{text}");
    assert!(
        text.contains("\"w\", \"Move the task to another stage\""),
        "{text}"
    );

    let painted = crate::ui::tests::text(&crate::ui::tests::render(120, 30, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(painted.contains("Keys · QA role"), "{painted}");

    press(&mut state, '?');
    assert!(state.dialog.is_none(), "? did not close the help");
}

// --- the Keys page -----------------------------------------------------------------

fn keys_page(config: &crate::config::ConfigHandle, id: &str) -> Dialog {
    let row = crate::ui::dialogs::key_settings::bindings(config)
        .iter()
        .position(|binding| binding.id == id)
        .unwrap_or_else(|| panic!("{id} is not on the page"));
    Dialog::Settings(SettingsDialog {
        page: Page::Keys,
        row,
        ..SettingsDialog::default()
    })
}

fn notice(dialog: &Dialog) -> String {
    match dialog {
        Dialog::Settings(settings) => settings.notice.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

#[test]
fn the_keys_page_sets_refuses_and_resets_a_key() {
    let (_dir, mut config, paths) = temp_config_paths();
    let mut dialog = keys_page(&config, "board.stage");

    press_dialog(&mut dialog, KeyCode::Enter, &mut config);
    press_dialog(&mut dialog, KeyCode::Char('r'), &mut config);
    assert!(
        notice(&dialog).contains("already refresh"),
        "{}",
        notice(&dialog)
    );
    assert!(config.key_overrides().is_empty(), "a clash was saved");

    press_dialog(&mut dialog, KeyCode::Enter, &mut config);
    press_dialog(&mut dialog, KeyCode::Char('w'), &mut config);
    let saved = crate::config::ConfigHandle::load(&paths, crate::config::EnvOverrides::default());
    assert_eq!(
        saved.key_overrides().get("board.stage").map(String::as_str),
        Some("w")
    );

    press_dialog(&mut dialog, KeyCode::Backspace, &mut config);
    assert!(config.key_overrides().is_empty());
    assert!(
        notice(&dialog).contains("back on `m`"),
        "{}",
        notice(&dialog)
    );
}

#[test]
fn the_keys_page_lists_only_the_role_s_keys_and_escape_keeps_the_old_key() {
    let (_dir, mut config, _paths) = temp_config_paths();
    config.set_role(UserRole::Qa).unwrap();
    let ids: Vec<&str> = crate::ui::dialogs::key_settings::bindings(&config)
        .iter()
        .map(|binding| binding.id)
        .collect();
    assert!(ids.contains(&"board.qa_run"));
    assert!(!ids.contains(&"board.pipeline"));
    assert!(!ids.contains(&"global.quit"), "a fixed key is listed");

    let mut dialog = keys_page(&config, "board.qa_run");
    press_dialog(&mut dialog, KeyCode::Enter, &mut config);
    press_dialog(&mut dialog, KeyCode::Esc, &mut config);
    assert!(config.key_overrides().is_empty());
    assert!(matches!(dialog, Dialog::Settings(_)), "Esc closed the page");
}

#[test]
fn comma_closes_settings_from_every_page() {
    let (_dir, mut config, _paths) = temp_config_paths();
    for page in [Page::Chat, Page::Qa, Page::Keys] {
        let mut dialog = Dialog::Settings(SettingsDialog {
            page,
            ..SettingsDialog::default()
        });
        let outcome = press_dialog(&mut dialog, KeyCode::Char(','), &mut config);
        assert!(
            matches!(outcome, crate::ui::dialogs::DialogOutcome::Close),
            "{page:?} stayed open"
        );
    }
}

#[test]
fn the_keys_page_draws_the_remapped_key_and_its_default() {
    let (_dir, mut config, _paths) = temp_config_paths();
    config.set_key("board.stage", Some('w')).unwrap();
    let Dialog::Settings(dialog) = keys_page(&config, "board.stage") else {
        unreachable!()
    };
    let buffer =
        crate::ui::tests::render_area(100, 40, |frame, area| dialog.render(frame, area, &config));
    let drawn = crate::ui::tests::text(&buffer);
    assert!(drawn.contains("Settings · Keys"), "{drawn}");
    assert!(drawn.contains("Move the task to another stage"), "{drawn}");
    assert!(drawn.contains("[w]  default m"), "{drawn}");
}

#[test]
fn the_conversation_pane_and_the_board_overlap() {
    assert!(Scope::Conversation.overlaps(Scope::Board));
    assert!(Scope::Global.overlaps(Scope::Deploy));
    assert!(!Scope::Board.overlaps(Scope::Deploy));
}
