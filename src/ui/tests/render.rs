//! What a frame looks like, where sessions come from, and suspend/restore.

use crossterm::event::KeyCode;

use crate::types::SessionStatus;
use crate::ui::app::{draw, status_hints};
use crate::ui::feed::{group_sessions, FeedEvent, SessionFeed, StaticFeed};
use crate::ui::run::{suspend, RecordingScreen, ScreenControl};
use crate::ui::state::{AppState, Pane, View};
use crate::ui::tests::{render, session, sessions_state, text};

const AREA: ratatui::layout::Rect = ratatui::layout::Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 30,
};

fn press(state: &mut AppState, code: KeyCode) {
    crate::ui::keys::handle_key(
        state,
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
        AREA,
    );
}

fn with_sessions(state: &mut AppState, sessions: Vec<crate::types::Session>) {
    state.apply_sessions(group_sessions(sessions), true);
}

fn three_sessions() -> Vec<crate::types::Session> {
    vec![
        session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle),
        session("bbb", "/Users/x/dev/alpha", SessionStatus::Working),
        session("ccc", "/Users/x/dev/beta", SessionStatus::Idle),
    ]
}

// --- feed seam --------------------------------------------------------------

#[test]
fn the_feed_seam_reports_refreshes_and_launches() {
    let feed = StaticFeed::default();
    feed.request_refresh();
    feed.note_launch();
    assert_eq!(feed.refreshes.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(feed.launches.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn a_feed_update_opens_projects_seen_for_the_first_time_only() {
    let (_dir, mut state) = sessions_state();
    let mut feed = StaticFeed::with(vec![FeedEvent::Sessions {
        by_project: group_sessions(three_sessions()),
        discovered: Default::default(),
        scan_complete: true,
    }]);
    for event in feed.drain() {
        if let FeedEvent::Sessions {
            by_project,
            scan_complete,
            ..
        } = event
        {
            state.apply_sessions(by_project, scan_complete);
        }
    }
    assert_eq!(state.stats.total_sessions, 3);
    assert_eq!(state.stats.total_projects, 2);
    assert!(state.expanded_projects.contains("x/alpha"));

    // Collapse it, then send the same list again: it stays collapsed.
    state.expanded_projects.remove("x/alpha");
    state.apply_sessions(group_sessions(three_sessions()), true);
    assert!(!state.expanded_projects.contains("x/alpha"));
}

// --- drawing ----------------------------------------------------------------

#[test]
fn the_sessions_view_draws_its_tree_and_status_hints() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("Claude Sessions Dashboard"), "{painted}");
    assert!(painted.contains("Sessions"), "{painted}");
    assert!(painted.contains("x/alpha"), "{painted}");
    assert!(painted.contains("Conversation"), "{painted}");
    assert!(painted.contains("Tab view"), "status bar: {painted}");

    // The bar is elided from the right on a narrow terminal, so the last hints
    // only appear when there is room for them.
    let wide = text(&render(200, 24, |frame| draw(frame, &mut state)));
    assert!(wide.contains("quit"), "{wide}");
    assert!(wide.contains("stop all"), "{wide}");
}

#[test]
fn an_empty_tree_says_so_rather_than_drawing_a_blank_pane() {
    let (_dir, mut state) = sessions_state();
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("No active sessions found"), "{painted}");
}

#[test]
fn killing_the_last_session_draws_the_empty_placeholder() {
    // The cursor sat on the session that was killed. A complete scan that
    // finds nothing must draw the normal empty pane, not the dead row under a
    // title that blames the scan.
    let (_dir, mut state) = sessions_state();
    with_sessions(
        &mut state,
        vec![session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle)],
    );
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Enter);
    state.take_actions();

    state.apply_sessions(group_sessions(Vec::new()), true);

    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("No active sessions found"), "{painted}");
    assert!(!painted.contains("showing the previous list"), "{painted}");
    assert!(!painted.contains("x/alpha"), "{painted}");
}

#[test]
fn an_incomplete_empty_scan_draws_the_previous_list_and_says_so() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());

    state.apply_sessions(group_sessions(Vec::new()), false);

    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("x/alpha"), "{painted}");
    assert!(painted.contains("showing the previous list"), "{painted}");
}

#[test]
fn the_deploy_tab_says_it_only_loads_when_asked() {
    // The one tab that never refreshes on its own: every load costs a GitLab
    // call per open merge request, so an empty pane has to say why.
    let (_dir, mut state) = sessions_state();
    state.view = View::Deploy;
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("Deploy"), "{painted}");
    assert!(painted.contains("Press r to load"), "{painted}");
}

#[test]
fn the_board_says_why_it_is_empty_rather_than_looking_broken() {
    // No credentials is a supported way to run the dashboard, so the tab has
    // to explain itself rather than showing an empty pane.
    let (_dir, mut state) = sessions_state();
    state.view = View::Board;
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("Tasks Board"), "{painted}");
    assert!(painted.contains("No Odoo credentials"), "{painted}");
}

#[test]
fn a_dialog_is_drawn_over_the_body() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    press(&mut state, KeyCode::Char('Q'));
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    assert!(painted.contains("Shut down"), "{painted}");
    assert!(painted.contains("Nothing is running"), "{painted}");
}

#[test]
fn only_the_visible_window_of_a_long_tree_is_drawn() {
    // Brief §10 mandate #7 — the pane cannot paint more rows than it has.
    let (_dir, mut state) = sessions_state();
    let many: Vec<crate::types::Session> = (0..500)
        .map(|i| {
            session(
                &format!("s{i:03}"),
                "/Users/x/dev/alpha",
                SessionStatus::Idle,
            )
        })
        .collect();
    with_sessions(&mut state, many);
    let painted = text(&render(100, 24, |frame| draw(frame, &mut state)));
    let drawn = painted.matches("idle").count();
    assert!(drawn <= 24, "painted {drawn} rows into a 24-row terminal");
}

#[test]
fn the_status_hints_change_with_the_focused_pane() {
    let (_dir, mut state) = sessions_state();
    let tree: Vec<&str> = status_hints(&state).iter().map(|(k, _)| *k).collect();
    state.focus = Pane::Conversation;
    let conv: Vec<&str> = status_hints(&state).iter().map(|(k, _)| *k).collect();
    assert!(!tree.contains(&"/"));
    assert!(conv.contains(&"/"), "{conv:?}");
    assert!(conv.contains(&"g/G"), "{conv:?}");
}

// --- suspend ----------------------------------------------------------------

#[test]
fn suspend_releases_the_terminal_and_takes_it_back() {
    // Node unmounted Ink entirely for the duration: leaving it mounted meant it
    // kept consuming stdin and repainting over the child.
    let mut screen = RecordingScreen::default();
    screen.acquire().expect("acquire");
    let mut ran = false;
    let result = suspend(&mut screen, || {
        ran = true;
        7
    })
    .expect("suspend");
    assert_eq!(result, 7);
    assert!(ran);
    assert!(screen.is_acquired(), "the terminal was not taken back");
    assert_eq!(screen.transitions, vec!["acquire", "release", "acquire"]);
}

#[test]
fn the_child_cannot_see_the_alternate_screen_while_it_runs() {
    let mut screen = RecordingScreen::default();
    screen.acquire().expect("acquire");
    let mut acquired_inside = true;
    suspend(&mut screen, || acquired_inside = false).expect("suspend");
    assert!(!acquired_inside);
}
