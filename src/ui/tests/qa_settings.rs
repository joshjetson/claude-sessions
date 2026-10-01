//! The settings dialog's QA page: every setting from today's work, without
//! opening the config file.

use crossterm::event::KeyCode;

use super::dialogs::{press, temp_config_paths};
use crate::config::{ConfigHandle, EnvOverrides};
use crate::ui::dialogs::qa_settings::{fields, QaField};
use crate::ui::dialogs::settings::Page;
use crate::ui::dialogs::{Dialog, SettingsDialog};

/// The QA page with the cursor on `field`.
fn on(config: &ConfigHandle, field: QaField) -> Dialog {
    let row = fields(config)
        .iter()
        .position(|f| *f == field)
        .expect("the field is on the page");
    Dialog::Settings(SettingsDialog {
        page: Page::Qa,
        row,
        ..SettingsDialog::default()
    })
}

fn notice(dialog: &Dialog) -> Option<String> {
    match dialog {
        Dialog::Settings(settings) => settings.notice.clone(),
        _ => None,
    }
}

/// The config as the daemon will read it: from the file.
fn reloaded(paths: &crate::paths::Paths) -> ConfigHandle {
    ConfigHandle::load(paths, EnvOverrides::default())
}

#[test]
fn tab_switches_between_the_chat_and_qa_pages() {
    let (_dir, mut config, _paths) = temp_config_paths();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    press(&mut dialog, KeyCode::Tab, &mut config);
    let Dialog::Settings(settings) = &dialog else {
        panic!("not the settings dialog");
    };
    assert_eq!(settings.page, Page::Qa);
    press(&mut dialog, KeyCode::Tab, &mut config);
    let Dialog::Settings(settings) = &dialog else {
        panic!("not the settings dialog");
    };
    assert_eq!(settings.page, Page::Chat);
}

#[test]
fn the_switches_save_to_the_file_the_daemon_reads() {
    let (_dir, mut config, paths) = temp_config_paths();
    for field in [
        QaField::HealthGate,
        QaField::NotifyNewInQa,
        QaField::AutoRefill,
        QaField::Alerts,
        QaField::CoordinatorMode,
    ] {
        let mut dialog = on(&config, field);
        press(&mut dialog, KeyCode::Right, &mut config);
    }
    let file = reloaded(&paths);
    assert!(file.qa_health_gate());
    assert!(file.qa_notify_new_in_qa());
    assert!(!file.qa_auto_refill());
    assert!(!file.alerts().enabled);
    assert_eq!(file.qa_coordinator_mode(), crate::qarun::RunMode::Shadow);
}

#[test]
fn the_role_and_the_lane_limit_step_and_wrap_or_stop() {
    let (_dir, mut config, _paths) = temp_config_paths();
    let mut dialog = on(&config, QaField::Role);
    press(&mut dialog, KeyCode::Right, &mut config);
    assert_eq!(config.role(), crate::types::UserRole::Qa);
    press(&mut dialog, KeyCode::Left, &mut config);
    press(&mut dialog, KeyCode::Left, &mut config);
    assert_eq!(config.role(), crate::types::UserRole::Pm, "did not wrap");

    let mut dialog = on(&config, QaField::LaneLimit);
    press(&mut dialog, KeyCode::Left, &mut config);
    assert_eq!(config.qa_lane_limit(), None, "went below none");
    press(&mut dialog, KeyCode::Right, &mut config);
    press(&mut dialog, KeyCode::Right, &mut config);
    assert_eq!(config.qa_lane_limit(), Some(2));
}

/// Auto QA starts with nobody at the keyboard, so a project with no folder or
/// several cannot be switched on, and the page says why.
#[test]
fn auto_qa_switches_only_a_project_with_one_folder() {
    let (_dir, mut config, paths) = temp_config_paths();
    config
        .add_odoo_project_dir("Aurora", "/tmp/aurora")
        .unwrap();
    config.add_odoo_project_dir("Borealis", "/tmp/b1").unwrap();
    config.add_odoo_project_dir("Borealis", "/tmp/b2").unwrap();

    let mut dialog = on(&config, QaField::AutoQa("Aurora".to_string()));
    press(&mut dialog, KeyCode::Right, &mut config);
    assert!(reloaded(&paths).qa_auto("Aurora"));

    let mut dialog = on(&config, QaField::AutoQa("Borealis".to_string()));
    press(&mut dialog, KeyCode::Right, &mut config);
    assert!(!config.qa_auto("Borealis"));
    assert!(notice(&dialog).is_some_and(|n| n.contains("exactly one repo folder")));
}

#[test]
fn peers_are_typed_as_a_list() {
    let (_dir, mut config, paths) = temp_config_paths();
    let mut dialog = on(&config, QaField::Peers);
    press(&mut dialog, KeyCode::Enter, &mut config);
    for ch in "100.95.137.24, cadendengel2".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    assert_eq!(
        reloaded(&paths).qa_peers().hosts,
        ["100.95.137.24", "cadendengel2"]
    );
}

/// A short secret is refused. `g` makes one, saves it, and shows it once so it
/// can be copied to the other machine. The row then shows only its end.
#[test]
fn a_peer_secret_is_refused_when_short_and_generated_with_g() {
    let (_dir, mut config, paths) = temp_config_paths();
    let mut dialog = on(&config, QaField::PeerSecret);
    press(&mut dialog, KeyCode::Enter, &mut config);
    for ch in "short".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    assert!(reloaded(&paths).qa_peers().secret.is_none());
    assert!(notice(&dialog).is_some_and(|n| n.contains("16 characters")));

    press(&mut dialog, KeyCode::Char('g'), &mut config);
    let secret = reloaded(&paths).qa_peers().secret.expect("a secret");
    assert_eq!(secret.len(), 32);
    assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(notice(&dialog).is_some_and(|n| n.contains(&secret)));
    let shown = QaField::PeerSecret.display(&config);
    assert!(
        shown.starts_with("••••") && !shown.contains(&secret[..28]),
        "{shown}"
    );
}

#[test]
fn the_peer_port_steps_and_the_default_is_not_written() {
    let (_dir, mut config, _paths) = temp_config_paths();
    let mut dialog = on(&config, QaField::PeerPort);
    press(&mut dialog, KeyCode::Right, &mut config);
    assert_eq!(config.qa_peers().port, 8789);
    press(&mut dialog, KeyCode::Left, &mut config);
    assert_eq!(config.qa_peers().port, 8788);
}

/// The board reads the role from the state on every key, so a role changed on
/// the page has to reach the state, not only the file.
#[test]
fn a_role_changed_on_the_page_applies_to_the_dashboard_at_once() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let (_dir, mut state) = crate::ui::tests::sessions_state();
    assert_eq!(state.role, crate::types::UserRole::Dev);
    state.dialog = Some(on(&state.config, QaField::Role));
    crate::ui::keys::handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        ratatui::layout::Rect::new(0, 0, 120, 40),
    );
    assert_eq!(state.role, crate::types::UserRole::Qa);
}

#[test]
fn the_qa_page_draws_its_sections_and_hides_the_secret() {
    let (_dir, mut config, _paths) = temp_config_paths();
    config
        .add_odoo_project_dir("Aurora", "/tmp/aurora")
        .unwrap();
    config
        .update_qa(|qa| qa.peer_secret = Some("0123456789abcdef0123456789abcdef".into()))
        .unwrap();
    let dialog = SettingsDialog {
        page: Page::Qa,
        ..SettingsDialog::default()
    };
    let buffer =
        crate::ui::tests::render_area(100, 40, |frame, area| dialog.render(frame, area, &config));
    let drawn = crate::ui::tests::text(&buffer);
    for expected in [
        "Settings · QA",
        "Health gate",
        "Auto QA",
        "Aurora",
        "Peer secret",
        "••••cdef",
        "apply when the daemon restarts",
    ] {
        assert!(drawn.contains(expected), "missing {expected:?}:\n{drawn}");
    }
    assert!(
        !drawn.contains("0123456789abcdef0123"),
        "the secret is on screen"
    );
}
