//! The four dialog primitives and the file viewer, on their own.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::Line;

use crate::ui::dialogs::widgets::{
    InlineChoice, ListOutcome, PromptOutcome, SelectList, TextPrompt,
};
use crate::ui::dialogs::{Dialog, DialogCtx, DialogOutcome, FileViewer};
use crate::ui::state::Action;
use crate::ui::tests::{render_area, temp_config, text};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(
    dialog: &mut Dialog,
    code: KeyCode,
    config: &mut crate::config::ConfigHandle,
) -> DialogOutcome {
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut ctx = DialogCtx { config };
    dialog.handle_key(key(code), area, &mut ctx)
}

// --- primitives -------------------------------------------------------------

#[test]
fn inline_choice_starts_on_cancel_so_enter_mashing_is_safe() {
    // THE property this widget exists for: put the safe option first and
    // hammering Enter through a destructive confirmation cancels.
    let mut choice = InlineChoice::new(["Cancel", "Delete everything"]);
    assert_eq!(choice.sel, 0);
    assert_eq!(
        choice.handle_key(key(KeyCode::Enter)),
        ListOutcome::Select(0)
    );
}

#[test]
fn inline_choice_escape_also_picks_the_safe_option() {
    let mut choice = InlineChoice::new(["Cancel", "Do it"]);
    choice.handle_key(key(KeyCode::Right));
    assert_eq!(choice.sel, 1);
    assert_eq!(choice.handle_key(key(KeyCode::Esc)), ListOutcome::Select(0));
}

#[test]
fn inline_choice_moves_with_arrows_hl_and_tab_and_clamps() {
    let mut choice = InlineChoice::new(["a", "b", "c"]);
    choice.handle_key(key(KeyCode::Char('l')));
    choice.handle_key(key(KeyCode::Tab));
    choice.handle_key(key(KeyCode::Right));
    assert_eq!(choice.sel, 2, "clamped at the last option");
    for _ in 0..5 {
        choice.handle_key(key(KeyCode::Char('h')));
    }
    assert_eq!(choice.sel, 0, "clamped at the first option");
}

#[test]
fn select_list_windows_to_the_visible_height() {
    let rows: Vec<Line<'static>> = (0..40).map(|i| Line::raw(format!("row {i}"))).collect();
    let mut list = SelectList::new(rows);
    // A 30-row body leaves 24 visible rows.
    let (top, visible) = list.window(30);
    assert_eq!((top, visible), (0, 24));
    list.sel = 30;
    let (top, visible) = list.window(30);
    assert_eq!(visible, 24);
    assert_eq!(top, 7, "the window follows the selection");
    assert_eq!(list.lines(30).len(), 24, "only the window is formatted");
}

#[test]
fn select_list_keeps_at_least_three_rows_on_a_short_terminal() {
    let rows: Vec<Line<'static>> = (0..40).map(|i| Line::raw(format!("row {i}"))).collect();
    let list = SelectList::new(rows);
    assert_eq!(list.window(4).1, 3);
}

#[test]
fn select_list_clamps_and_reports_unhandled_keys() {
    let mut list = SelectList::new(vec![Line::raw("a"), Line::raw("b")]);
    list.handle_key(key(KeyCode::Up));
    assert_eq!(list.sel, 0);
    for _ in 0..5 {
        list.handle_key(key(KeyCode::Char('j')));
    }
    assert_eq!(list.sel, 1);
    assert_eq!(list.handle_key(key(KeyCode::Enter)), ListOutcome::Select(1));
    assert_eq!(list.handle_key(key(KeyCode::Esc)), ListOutcome::Cancel);
    // The extra-key hook: anything the list does not use comes back out.
    assert_eq!(
        list.handle_key(key(KeyCode::Char('x'))),
        ListOutcome::Unhandled(key(KeyCode::Char('x')))
    );
}

#[test]
fn text_prompt_submits_on_enter_when_single_line() {
    let mut prompt = TextPrompt::new("", false);
    for ch in "hello".chars() {
        prompt.handle_key(key(KeyCode::Char(ch)));
    }
    assert_eq!(
        prompt.handle_key(key(KeyCode::Enter)),
        PromptOutcome::Submit("hello".into())
    );
}

#[test]
fn a_multiline_prompt_takes_enter_as_a_newline_and_submits_on_ctrl_s() {
    let mut prompt = TextPrompt::new("", true);
    prompt.handle_key(key(KeyCode::Char('a')));
    assert_eq!(prompt.handle_key(key(KeyCode::Enter)), PromptOutcome::Stay);
    prompt.handle_key(key(KeyCode::Char('b')));
    assert_eq!(
        prompt.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        PromptOutcome::Submit("a\nb".into())
    );
}

#[test]
fn a_control_chord_is_never_typed_into_the_field() {
    let mut prompt = TextPrompt::new("", false);
    prompt.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(prompt.value, "");
}

#[test]
fn text_prompt_hard_wraps_and_keeps_the_cursor_on_the_last_line() {
    let mut prompt = TextPrompt::new("", false);
    for ch in "abcdefghij".chars() {
        prompt.handle_key(key(KeyCode::Char(ch)));
    }
    let wrapped = prompt.wrapped(4, 10);
    assert_eq!(wrapped, vec!["abcd", "efgh", "ij█"]);
}

#[test]
fn text_prompt_tails_to_the_cursor_when_it_outgrows_the_box() {
    let mut prompt = TextPrompt::new("", false);
    for ch in "abcdefghijkl".chars() {
        prompt.handle_key(key(KeyCode::Char(ch)));
    }
    let wrapped = prompt.wrapped(4, 2);
    assert_eq!(wrapped, vec!["ijkl", "█"], "the cursor stays visible");
}

#[test]
fn text_prompt_escape_cancels_without_saving() {
    let mut prompt = TextPrompt::new("keep me", false);
    assert_eq!(prompt.handle_key(key(KeyCode::Esc)), PromptOutcome::Cancel);
}

// --- file viewer ------------------------------------------------------------

#[test]
fn the_file_viewer_caches_by_mtime_rather_than_re_reading_per_keystroke() {
    // Brief §10 mandate #8. Node called readFileSync inside render, so holding
    // `j` re-read the whole file once per frame.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.md");
    std::fs::write(&path, "one\ntwo\nthree\n").expect("write");
    let mut viewer = FileViewer::open(" Log ", &path);
    assert_eq!(viewer.len(), 3);
    assert_eq!(viewer.reads(), 1);

    // A hundred keystrokes over an unchanged file: still one read.
    for _ in 0..100 {
        viewer.reload_if_changed();
    }
    assert_eq!(viewer.reads(), 1, "the file was re-read while unchanged");

    // A file that really did change is picked up. The mtime is set explicitly so
    // the test does not depend on the filesystem's timestamp resolution.
    std::fs::write(&path, "one\n").expect("rewrite");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_times(std::fs::FileTimes::new().set_modified(later))
        .expect("set mtime");
    viewer.reload_if_changed();
    assert_eq!(viewer.reads(), 2);
    assert_eq!(viewer.len(), 1);
}

#[test]
fn a_missing_file_reads_as_a_message_not_a_crash() {
    let viewer = FileViewer::open(" Log ", "/nonexistent/path.log");
    assert_eq!(viewer.len(), 1);
    let buffer = render_area(80, 20, |frame, area| viewer.render(frame, area));
    assert!(text(&buffer).contains("could not read file"));
}

#[test]
fn the_file_viewer_opens_at_the_bottom_and_g_goes_to_the_top() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.md");
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, body).expect("write");
    let mut viewer = FileViewer::open(" Log ", &path);
    let area = ratatui::layout::Rect::new(0, 0, 80, 30);
    assert_eq!(viewer.scroll, None, "None means stick to the bottom");
    let buffer = render_area(80, 30, |frame, area| viewer.render(frame, area));
    assert!(text(&buffer).contains("line 199"));

    viewer.handle_key(key(KeyCode::Char('g')), area);
    assert_eq!(viewer.scroll, Some(0));
    let buffer = render_area(80, 30, |frame, area| viewer.render(frame, area));
    assert!(text(&buffer).contains("line 0"));
}

#[test]
fn the_file_viewer_hands_opening_the_file_back_as_an_action() {
    // Opening an editor means suspending the dashboard, which only the loop can
    // do — so the viewer asks rather than spawning.
    let (_dir, mut config) = temp_config();
    let mut dialog = Dialog::FileViewer(FileViewer::open(" Log ", "/tmp/x.log"));
    match press(&mut dialog, KeyCode::Char('o'), &mut config) {
        DialogOutcome::Act(Action::OpenEditor { path, .. }) => assert_eq!(path, "/tmp/x.log"),
        other => panic!("expected an editor action, got {other:?}"),
    }
}

#[test]
fn key_releases_are_ignored_so_a_keystroke_is_not_counted_twice() {
    let (_dir, mut state) = crate::ui::tests::temp_state();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    crate::ui::keys::handle_key(&mut state, release, area);
    assert!(state.quit.is_none());
}
