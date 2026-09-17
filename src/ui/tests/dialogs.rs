//! Dialogs: the smoke matrix, the primitives, and the shutdown confirmation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::ConfigHandle;
use crate::paths::Paths;
use crate::types::SessionStatus;
use crate::ui::dialogs::{
    AddGroup, DaemonLogs, Dialog, DialogCtx, DialogOutcome, FileViewer, KillConfirm, LogViewer,
    PurgeConfirm, Rename, Search, SettingsDialog, ShutdownConfirm,
};
use crate::ui::state::Action;
use crate::ui::tests::{render_area, session, temp_config, text};
use crate::ui::tree::{SelectedRow, SessionsByProject};

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn press(
    dialog: &mut Dialog,
    code: KeyCode,
    config: &mut ConfigHandle,
) -> DialogOutcome {
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut ctx = DialogCtx { config };
    dialog.handle_key(key(code), area, &mut ctx)
}

/// A throwaway config plus the paths that go with it — the log and purge
/// dialogs read the runtime directory, so they need both.
pub(super) fn temp_config_paths() -> (tempfile::TempDir, ConfigHandle, Paths) {
    let (dir, config) = temp_config();
    let paths = Paths::for_test(dir.path());
    (dir, config, paths)
}

/// Every dialog, in the state it opens in.
fn every_dialog(config: &ConfigHandle, paths: &Paths) -> Vec<Dialog> {
    let row = SelectedRow::Session {
        session_id: "abcd1234".into(),
        pids: vec![4242],
    };
    vec![
        Dialog::Kill(KillConfirm::for_row(
            Some(&row),
            &SessionsByProject::new(),
            config,
        )),
        Dialog::Rename(Rename::new("abcd1234", config)),
        Dialog::AddGroup(AddGroup::new()),
        Dialog::Search(Search::new(config)),
        Dialog::Settings(SettingsDialog::default()),
        Dialog::Shutdown(ShutdownConfirm::default()),
        Dialog::FileViewer(FileViewer::open(" Log ", "/nonexistent/for/the/test.log")),
        Dialog::LogViewer(Box::new(LogViewer::open(paths, None, "2026-09-16"))),
        Dialog::PurgeConfirm(PurgeConfirm::new(Vec::new(), Default::default())),
        Dialog::DaemonLogs(DaemonLogs::open(&paths.auto_dev_runs_dir, 5944, &[])),
    ]
}

#[test]
fn the_smoke_matrix_renders_every_dialog() {
    // This is the cheapest high-value test the Node suite had: `P` once shipped
    // with a ReferenceError that only fired at RENDER time, so every check that
    // merely imported the module passed while pressing the key wedged the
    // dashboard. Rendering each dialog once catches that whole class.
    let (_dir, config, paths) = temp_config_paths();
    for mut dialog in every_dialog(&config, &paths) {
        let name = dialog.name();
        let buffer = render_area(100, 30, |frame, area| dialog.render(frame, area, &config));
        let painted = text(&buffer);
        assert!(
            painted.chars().any(|c| c != ' '),
            "{name} rendered a blank frame"
        );
    }
}

#[test]
fn every_dialog_survives_a_burst_of_keys() {
    // The other half of the matrix: a dialog that panics on Esc-before-load, or
    // on a key it does not know, is just as wedged as one that will not render.
    let (_dir, mut config, paths) = temp_config_paths();
    for mut dialog in every_dialog(&config, &paths) {
        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('z'),
            KeyCode::Backspace,
            KeyCode::PageDown,
            KeyCode::Tab,
        ] {
            press(&mut dialog, code, &mut config);
        }
    }
}

#[test]
fn every_dialog_closes_on_escape() {
    let (_dir, mut config, paths) = temp_config_paths();
    for mut dialog in every_dialog(&config, &paths) {
        let name = dialog.name();
        let outcome = press(&mut dialog, KeyCode::Esc, &mut config);
        assert!(
            matches!(outcome, DialogOutcome::Close | DialogOutcome::Act(_)),
            "{name} did not close on Esc: {outcome:?}"
        );
    }
}

#[test]
fn a_dialog_swallows_input_before_the_view_sees_it() {
    // `q` quits the dashboard everywhere else; inside a confirmation it must not.
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Kill(KillConfirm {
        pids: vec![1],
        label: "one".into(),
    });
    assert_eq!(
        press(&mut dialog, KeyCode::Char('q'), &mut config),
        DialogOutcome::Stay
    );
}

// --- kill -------------------------------------------------------------------

#[test]
fn kill_enqueues_rather_than_signalling_from_the_draw_thread() {
    // Brief §10 mandate #9: the UI thread never blocks on a process. The dialog
    // hands back an Action; the worker is what touches `kill`.
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Kill(KillConfirm {
        pids: vec![4242, 4243],
        label: "session abcd".into(),
    });
    match press(&mut dialog, KeyCode::Enter, &mut config) {
        DialogOutcome::Act(Action::Kill { pids, label }) => {
            assert_eq!(pids, vec![4242, 4243]);
            assert_eq!(label, "session abcd");
        }
        other => panic!("expected a kill action, got {other:?}"),
    }
}

#[test]
fn kill_with_nothing_to_kill_says_so_and_does_nothing() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Kill(KillConfirm::for_row(
        None,
        &SessionsByProject::new(),
        &config,
    ));
    let buffer = render_area(80, 20, |frame, area| dialog.render(frame, area, &config));
    assert!(text(&buffer).contains("No running processes to kill"));
    assert_eq!(
        press(&mut dialog, KeyCode::Enter, &mut config),
        DialogOutcome::Close
    );
}

#[test]
fn killing_a_project_row_gathers_every_pid_in_it() {
    let (_dir, config) = temp_config();
    let mut alpha = session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle);
    alpha.pids = vec![1, 2];
    let mut beta = session("bbb", "/Users/x/dev/alpha", SessionStatus::Idle);
    beta.pids = vec![3];
    let by_project = crate::ui::feed::group_sessions(vec![alpha, beta]);
    let dialog = KillConfirm::for_row(
        Some(&SelectedRow::Project {
            name: "x/alpha".into(),
        }),
        &by_project,
        &config,
    );
    assert_eq!(dialog.pids, vec![1, 2, 3]);
    assert!(dialog.label.contains("all 2 session(s) in x/alpha"));
}

// --- rename / search / add group --------------------------------------------

#[test]
fn rename_saves_the_nickname_and_an_empty_one_clears_it() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Rename(Rename::new("abcd1234", &config));
    for ch in "migration notes".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    assert_eq!(config.session_nickname("abcd1234"), Some("migration notes"));

    let mut again = Dialog::Rename(Rename::new("abcd1234", &config));
    for _ in 0.."migration notes".len() {
        press(&mut again, KeyCode::Backspace, &mut config);
    }
    press(&mut again, KeyCode::Enter, &mut config);
    assert_eq!(config.session_nickname("abcd1234"), None);
}

#[test]
fn search_saves_the_keyword_trimmed() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Search(Search::new(&config));
    for ch in " pairing ".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    assert_eq!(config.chat().search_keyword, "pairing");
}

#[test]
fn add_group_asks_for_a_name_then_a_path_and_resets_between_them() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::AddGroup(AddGroup::new());
    for ch in "Work".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);

    // The second prompt must NOT inherit the typed group name — Node needed
    // distinct React keys here for exactly this reason.
    let Dialog::AddGroup(inner) = &dialog else {
        panic!("not an add-group dialog")
    };
    assert_eq!(inner.prompt.value, "~/");
    let buffer = render_area(80, 20, |frame, area| dialog.render(frame, area, &config));
    assert!(text(&buffer).contains("Work"), "the title names the group");

    for ch in "code".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    let outcome = press(&mut dialog, KeyCode::Enter, &mut config);
    assert_eq!(outcome, DialogOutcome::Act(Action::Refresh));
    assert_eq!(config.groups().len(), 1);
    assert_eq!(config.groups()[0].name, "Work");
    // `add_group` expands the leading tilde on save, so the stored path is
    // absolute — the prompt is where `~/` is a convenience, not the file.
    assert!(
        config.groups()[0].path.ends_with("/code"),
        "{}",
        config.groups()[0].path
    );
}

#[test]
fn add_group_with_an_empty_name_just_closes() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::AddGroup(AddGroup::new());
    assert_eq!(
        press(&mut dialog, KeyCode::Enter, &mut config),
        DialogOutcome::Close
    );
    assert!(config.groups().is_empty());
}

// --- settings ---------------------------------------------------------------

#[test]
fn settings_cycles_an_enum_with_the_arrow_keys_and_saves() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    assert_eq!(config.chat().theme, "default");
    press(&mut dialog, KeyCode::Right, &mut config);
    assert_eq!(config.chat().theme, "solarized");
    press(&mut dialog, KeyCode::Left, &mut config);
    assert_eq!(config.chat().theme, "default");
    // Cycling backwards past the start wraps to the end.
    press(&mut dialog, KeyCode::Left, &mut config);
    assert_eq!(config.chat().theme, "minimal");
}

#[test]
fn cycling_the_theme_writes_its_colours_into_the_config() {
    // Without this, `resolve_theme`'s "differs from the base" rule would leave
    // the old colours in place and the theme would appear not to apply.
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    press(&mut dialog, KeyCode::Right, &mut config); // -> solarized
    assert_eq!(config.chat().theme, "solarized");
    assert_eq!(config.chat().user_color, "blue");
    assert_eq!(config.chat().code_color, "cyan");
}

#[test]
fn settings_clamps_its_number_fields() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    // Walk to "Panel width" and drive it past both ends.
    let fields = crate::ui::dialogs::settings::editable();
    let width_row = fields
        .iter()
        .position(|f| *f == crate::ui::dialogs::settings::Field::ConversationWidth)
        .expect("panel width row");
    for _ in 0..width_row {
        press(&mut dialog, KeyCode::Down, &mut config);
    }
    for _ in 0..40 {
        press(&mut dialog, KeyCode::Left, &mut config);
    }
    assert_eq!(config.chat().conversation_width, 10);
    for _ in 0..40 {
        press(&mut dialog, KeyCode::Right, &mut config);
    }
    assert_eq!(config.chat().conversation_width, 80);
}

#[test]
fn settings_edits_a_text_field_on_enter() {
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    let fields = crate::ui::dialogs::settings::editable();
    let label_row = fields
        .iter()
        .position(|f| *f == crate::ui::dialogs::settings::Field::UserLabel)
        .expect("user label row");
    for _ in 0..label_row {
        press(&mut dialog, KeyCode::Down, &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    for _ in 0.."You".len() {
        press(&mut dialog, KeyCode::Backspace, &mut config);
    }
    for ch in "Me".chars() {
        press(&mut dialog, KeyCode::Char(ch), &mut config);
    }
    press(&mut dialog, KeyCode::Enter, &mut config);
    assert_eq!(config.chat().user_label, "Me");
}

#[test]
fn the_settings_grid_lists_every_row_and_its_separators() {
    let (_dir, config) = temp_config();
    let mut dialog = Dialog::Settings(SettingsDialog::default());
    let buffer = render_area(100, 30, |frame, area| dialog.render(frame, area, &config));
    let painted = text(&buffer);
    for label in [
        "Theme",
        "User color",
        "Tool color",
        "User label",
        "Timestamps",
        "Tool calls",
        "Compact mode",
        "Max lines/msg",
        "Show messages",
        "Panel width",
        "Swap panels",
        "Session header",
    ] {
        assert!(painted.contains(label), "missing row {label}");
    }
    assert!(painted.contains("Display"));
    assert!(painted.contains("Layout"));
    assert_eq!(crate::ui::dialogs::settings::editable().len(), 15);
}
