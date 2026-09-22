//! Key routing, selection stability, the action seam, and suspend/restore.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::types::SessionStatus;
use crate::ui::actions::KILL_REFRESH_DELAYS;
use crate::ui::feed::group_sessions;
use crate::ui::keys::{handle_key, tree_snapshot};
use crate::ui::state::{Action, AppState, Pane, Quit, Selection, View};
use crate::ui::tests::{session, sessions_state, temp_state};

const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 30,
};

fn press(state: &mut AppState, code: KeyCode) {
    handle_key(state, KeyEvent::new(code, KeyModifiers::NONE), AREA);
}

fn with_sessions(state: &mut AppState, sessions: Vec<crate::types::Session>) {
    state.apply_sessions(group_sessions(sessions));
}

fn three_sessions() -> Vec<crate::types::Session> {
    vec![
        session("aaa", "/Users/x/dev/alpha", SessionStatus::Idle),
        session("bbb", "/Users/x/dev/alpha", SessionStatus::Working),
        session("ccc", "/Users/x/dev/beta", SessionStatus::Idle),
    ]
}

// --- selection --------------------------------------------------------------

#[test]
fn a_keyed_selection_survives_a_reshuffle() {
    // THE property: a session finishing above the cursor must not move the
    // cursor. Selection is by identity, not by index.
    let mut selection = Selection::default();
    let before: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    selection.set(&before, 2);
    assert_eq!(selection.resolve(&before), 2);

    let after: Vec<String> = ["c", "a", "b"].iter().map(|s| s.to_string()).collect();
    assert_eq!(selection.resolve(&after), 0, "followed its key");
}

#[test]
fn a_vanished_key_falls_back_to_the_stored_position() {
    let mut selection = Selection::default();
    let before: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
    selection.set(&before, 2);
    let after: Vec<String> = ["a", "b", "x", "d"].iter().map(|s| s.to_string()).collect();
    assert_eq!(selection.resolve(&after), 2, "positional fallback");
}

#[test]
fn the_fallback_clamps_when_the_list_shrinks() {
    let mut selection = Selection::default();
    let before: Vec<String> = (0..10).map(|i| i.to_string()).collect();
    selection.set(&before, 9);
    let after: Vec<String> = (0..3).map(|i| format!("x{i}")).collect();
    assert_eq!(selection.resolve(&after), 2);
    assert_eq!(selection.resolve(&[]), 0, "an empty list selects nothing");
}

#[test]
fn moving_the_cursor_records_the_new_key() {
    let mut selection = Selection::default();
    let keys: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    selection.move_by(&keys, 1);
    assert_eq!(selection.key(), Some("b"));
    selection.move_by(&keys, 10);
    assert_eq!(selection.key(), Some("c"), "clamped at the end");
    selection.move_by(&keys, -10);
    assert_eq!(selection.key(), Some("a"), "clamped at the start");
}

#[test]
fn the_cursor_stays_on_its_session_when_the_list_reorders_between_ticks() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    // alpha is expanded on first sight, so its two sessions are rows.
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Down);
    let before = tree_snapshot(&state);
    let selected_key = before.keys[before.selected].clone();

    // The scan comes back with the same sessions in a different order.
    let mut reordered = three_sessions();
    reordered.reverse();
    for (i, s) in reordered.iter_mut().enumerate() {
        s.last_timestamp = Some(format!("2026-09-16T1{i}:00:00.000Z"));
    }
    with_sessions(&mut state, reordered);

    let after = tree_snapshot(&state);
    assert_eq!(after.keys[after.selected], selected_key);
}

// --- global keys ------------------------------------------------------------

#[test]
fn the_opening_view_comes_from_config() {
    // Config's own default is the board; that is what a fresh install opens on.
    let (_dir, state) = temp_state();
    assert_eq!(state.view, View::Board);
}

#[test]
fn q_and_ctrl_c_detach_without_stopping_the_daemon() {
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('q'));
    assert_eq!(state.quit, Some(Quit::Detach));

    let (_dir, mut state) = sessions_state();
    handle_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        AREA,
    );
    assert_eq!(state.quit, Some(Quit::Detach));
}

#[test]
fn shift_q_confirms_before_stopping_everything() {
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('Q'));
    assert!(state.quit.is_none(), "Shift-Q must confirm first");
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("shutdown"));
}

#[test]
fn tab_cycles_the_view_and_shift_tab_cycles_the_pane() {
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.view, View::Board);
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.view, View::Deploy);
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.view, View::Sessions);

    assert_eq!(state.focus, Pane::Tree);
    press(&mut state, KeyCode::BackTab);
    assert_eq!(state.focus, Pane::Conversation);
    press(&mut state, KeyCode::BackTab);
    assert_eq!(state.focus, Pane::Tree);
}

#[test]
fn r_refreshes_everywhere_except_on_a_session_row() {
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('r'));
    assert_eq!(state.pending_actions().front(), Some(&Action::Refresh));

    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    press(&mut state, KeyCode::Down); // onto a session row
    press(&mut state, KeyCode::Char('r'));
    assert!(state.take_actions().is_empty(), "r renamed instead");
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("rename"));
}

#[test]
fn u_queues_a_usage_check_and_says_it_is_spending_one() {
    // A key that silently does nothing reads as a broken dashboard, and this
    // one costs a request against the quota it reports — so it says so.
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('u'));
    assert!(state
        .pending_actions()
        .iter()
        .any(|action| matches!(action, Action::RefreshUsage)));
    let flash = state.flash.clone().unwrap_or_default();
    assert!(flash.contains("usage"), "{flash}");
}

#[test]
fn l_and_shift_l_both_open_the_daily_log() {
    for code in [KeyCode::Char('l'), KeyCode::Char('L')] {
        let (_dir, mut state) = sessions_state();
        press(&mut state, code);
        assert_eq!(
            state.dialog.as_ref().map(crate::ui::dialogs::Dialog::name),
            Some("logViewer"),
            "{code:?}"
        );
        // The key that opened it shuts it again.
        press(&mut state, code);
        assert!(state.dialog.is_none(), "{code:?} did not close it");
    }
}

#[test]
fn shift_x_opens_the_purge_confirmation_rather_than_purging() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    press(&mut state, KeyCode::Char('X'));
    assert_eq!(
        state.dialog.as_ref().map(crate::ui::dialogs::Dialog::name),
        Some("purgeConfirm")
    );
    // Nothing is killed by opening it.
    assert!(!state
        .pending_actions()
        .iter()
        .any(|action| matches!(action, Action::Purge(_))));
}

// --- sessions view ----------------------------------------------------------

#[test]
fn enter_toggles_a_project_and_arrows_expand_and_collapse_it() {
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    assert!(state.expanded_projects.contains("x/alpha"));
    press(&mut state, KeyCode::Enter);
    assert!(!state.expanded_projects.contains("x/alpha"));
    press(&mut state, KeyCode::Right);
    assert!(state.expanded_projects.contains("x/alpha"));
    press(&mut state, KeyCode::Left);
    assert!(!state.expanded_projects.contains("x/alpha"));
}

#[test]
fn selecting_a_session_enqueues_the_transcript_read() {
    // Opening a transcript is I/O, so the key handler asks rather than reads.
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Enter);
    assert_eq!(state.selected_session_id.as_deref(), Some("aaa"));
    match state.take_actions().first() {
        Some(Action::SelectSession { session_id, .. }) => assert_eq!(session_id, "aaa"),
        other => panic!("expected a select action, got {other:?}"),
    }
}

#[test]
fn o_and_n_and_x_all_go_through_the_action_queue() {
    // Brief §10 mandate #9: the draw thread must never block on a spawn. Every
    // one of these leaves as an action for the worker.
    let (_dir, mut state) = sessions_state();
    with_sessions(&mut state, three_sessions());
    press(&mut state, KeyCode::Down); // a session row

    press(&mut state, KeyCode::Char('o'));
    assert!(matches!(
        state.take_actions().first(),
        Some(Action::FocusTerminal(_))
    ));

    press(&mut state, KeyCode::Char('n'));
    match state.take_actions().first() {
        Some(Action::LaunchSession { cwd }) => assert_eq!(cwd, "/Users/x/dev/alpha"),
        other => panic!("expected a launch action, got {other:?}"),
    }

    press(&mut state, KeyCode::Char('x'));
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("killConfirm"));
    assert!(
        state.take_actions().is_empty(),
        "x must confirm before signalling anything"
    );
}

#[test]
fn a_kill_refreshes_three_times_because_sigterm_is_not_instant() {
    assert_eq!(KILL_REFRESH_DELAYS.len(), 3);
    assert_eq!(KILL_REFRESH_DELAYS[0].as_millis(), 0);
    assert_eq!(KILL_REFRESH_DELAYS[1].as_millis(), 400);
    assert_eq!(KILL_REFRESH_DELAYS[2].as_millis(), 1000);
}

#[test]
fn a_and_s_open_their_dialogs() {
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('a'));
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("addGroup"));
    let (_dir, mut state) = sessions_state();
    press(&mut state, KeyCode::Char('s'));
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("settings"));
}

#[test]
fn d_removes_a_group_only_on_its_separator() {
    let (_dir, mut state) = sessions_state();
    state
        .config
        .add_group("Work", "/Users/x/work")
        .expect("add group");
    with_sessions(
        &mut state,
        vec![session("aaa", "/Users/x/work/alpha", SessionStatus::Idle)],
    );
    // The separator is the first row.
    press(&mut state, KeyCode::Char('d'));
    assert!(state.config.groups().is_empty());
}

// --- conversation pane ------------------------------------------------------

#[test]
fn conversation_keys_only_apply_when_that_pane_has_focus() {
    let (_dir, mut state) = sessions_state();
    assert!(!state.config.chat().show_timestamps);
    press(&mut state, KeyCode::Char('t')); // tree focus: no effect
    assert!(!state.config.chat().show_timestamps);
    state.focus = Pane::Conversation;
    press(&mut state, KeyCode::Char('t'));
    assert!(state.config.chat().show_timestamps);
}

#[test]
fn f_cycles_the_message_filter() {
    let (_dir, mut state) = sessions_state();
    state.focus = Pane::Conversation;
    for expected in ["user", "assistant", "all"] {
        press(&mut state, KeyCode::Char('f'));
        assert_eq!(state.config.chat().message_filter, expected);
    }
}

#[test]
fn slash_opens_search_and_s_opens_settings_from_the_conversation() {
    let (_dir, mut state) = sessions_state();
    state.focus = Pane::Conversation;
    press(&mut state, KeyCode::Char('/'));
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("search"));
    let (_dir, mut state) = sessions_state();
    state.focus = Pane::Conversation;
    press(&mut state, KeyCode::Char('s'));
    assert_eq!(state.dialog.as_ref().map(|d| d.name()), Some("settings"));
}

#[test]
fn scrolling_releases_the_stick_and_g_capital_restores_it() {
    let (_dir, mut state) = sessions_state();
    state.focus = Pane::Conversation;
    state.conv.page_height = 10;
    state.conv.total_lines = 100;
    state.conv.stick = true;
    state.conv.scroll_top = 90;

    press(&mut state, KeyCode::Up);
    assert_eq!(state.conv.scroll_top, 89);
    assert!(!state.conv.stick, "scrolling up released the stick");

    press(&mut state, KeyCode::Char('g'));
    assert_eq!(state.conv.scroll_top, 0);
    assert!(!state.conv.stick);

    press(&mut state, KeyCode::Char('G'));
    assert!(state.conv.stick, "G re-sticks to the bottom");
}

#[test]
fn page_keys_move_by_the_last_measured_page() {
    let (_dir, mut state) = sessions_state();
    state.focus = Pane::Conversation;
    state.conv.page_height = 10;
    state.conv.total_lines = 100;
    state.conv.scroll_top = 50;
    press(&mut state, KeyCode::PageUp);
    assert_eq!(state.conv.scroll_top, 40);
    press(&mut state, KeyCode::Char(' '));
    assert_eq!(state.conv.scroll_top, 50);
    for _ in 0..20 {
        press(&mut state, KeyCode::PageDown);
    }
    assert_eq!(state.conv.scroll_top, 90, "clamped at the last page");
    assert!(state.conv.stick, "reaching the bottom re-sticks");
}

#[test]
fn a_failed_usage_check_keeps_the_last_numbers_and_marks_them_stale() {
    // The readout must not blink out whenever a check times out — the header
    // keeps what it had and says the numbers are old.
    let (_dir, mut state) = sessions_state();
    state.apply_usage(crate::usage::UsageSnapshot {
        ok: true,
        session: Some(42.0),
        week: Some(55.0),
        ..crate::usage::UsageSnapshot::default()
    });
    state.apply_usage(crate::usage::UsageSnapshot {
        ok: false,
        error: Some("claude did not answer".into()),
        ..crate::usage::UsageSnapshot::default()
    });
    let usage = state.usage.clone().expect("a readout");
    assert_eq!(usage.session, Some(42.0));
    assert!(!usage.ok);
    assert!(usage.format(80).unwrap().contains("(stale)"));
}

// --- opening a run's coordinator ---------------------------------------------

/// A state with one run whose coordinator is running.
fn with_a_run(coordinator: bool) -> AppState {
    let mut state = temp_state().1;
    let run_id = "x/alpha::Quality Assurance".to_string();

    // Distinct pids, and the agent carries the task the run covers: that is how
    // a run finds its agents, and a fixture without it makes the assertions
    // below pass for the wrong reason.
    let mut agent = session("agent", "/Users/x/dev/alpha", SessionStatus::Working);
    agent.task_id = Some(4101);
    agent.pids = vec![101];
    let mut sessions = vec![agent];
    if coordinator {
        let mut coord = session("coord", "/Users/x/dev/alpha", SessionStatus::Idle);
        coord.run_id = Some(run_id.clone());
        coord.pids = vec![202];
        sessions.push(coord);
    }
    with_sessions(&mut state, sessions);

    state.board.runs.push(crate::qarun::QaRun {
        id: run_id,
        project_name: "x/alpha".to_string(),
        stage_name: "Quality Assurance".to_string(),
        task_ids: vec![4101],
        started_at: String::new(),
        lane_limit: None,
        spawned: Vec::new(),
        mode: crate::qarun::RunMode::default(),
        coordinator_started: true,
    });
    state.view = View::Sessions;
    state.focus = Pane::Tree;
    state.take_actions();
    state
}

/// Put the cursor on the run row.
fn on_the_run_row(state: &mut AppState) {
    let snapshot = tree_snapshot(state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("r:"))
        .expect("a run row");
    state.tree_sel.set(&snapshot.keys, index);
}

#[test]
fn enter_on_a_run_row_opens_the_coordinators_conversation() {
    // The row IS the coordinator, so the key that opens a session opens it.
    // Before this, Enter only expanded the run and the coordinator had no way
    // in at all — the one row you most want to open did nothing.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Enter);

    assert_eq!(
        state.selected_session_id.as_deref(),
        Some("coord"),
        "Enter did not select the coordinator"
    );
    assert!(
        state.take_actions().iter().any(
            |action| matches!(action, Action::SelectSession { session_id, .. }
                if session_id == "coord")
        ),
        "no SelectSession action was queued for the coordinator"
    );
}

#[test]
fn enter_on_a_run_row_also_expands_it() {
    // Opening the conversation and showing the agents are the same request.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Enter);

    assert!(
        state
            .expanded_projects
            .iter()
            .any(|key| key.starts_with("r:")),
        "the run did not expand"
    );
}

#[test]
fn o_on_a_run_row_focuses_the_coordinators_terminal() {
    // It used to fall through to whatever was selected last, which is a
    // different session than the row the key was pressed on.
    let mut state = with_a_run(true);
    state.selected_session_id = Some("agent".to_string());
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('o'));

    let focused = state
        .take_actions()
        .into_iter()
        .find_map(|action| match action {
            Action::FocusTerminal(reference) => reference.session_id.clone(),
            _ => None,
        });
    assert_eq!(
        focused.as_deref(),
        Some("coord"),
        "'o' focused the wrong session"
    );
}

#[test]
fn a_run_with_no_coordinator_says_so_rather_than_doing_nothing() {
    // A key that silently does nothing reads as broken. It is not broken — the
    // run has no coordinator running — and the row should say which.
    let mut state = with_a_run(false);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Enter);

    assert!(
        state
            .flash
            .as_deref()
            .is_some_and(|text| text.contains("no coordinator")),
        "nothing was said: {:?}",
        state.flash
    );
    assert_eq!(state.selected_session_id, None);
}

// --- killing a whole run ------------------------------------------------------

fn kill_dialog(state: &AppState) -> &crate::ui::dialogs::KillConfirm {
    match &state.dialog {
        Some(crate::ui::dialogs::Dialog::Kill(confirm)) => confirm,
        other => panic!("expected a kill dialog, got {other:?}"),
    }
}

#[test]
fn x_on_a_run_row_collects_the_coordinator_and_every_agent() {
    // Killing them one at a time leaves the coordinator watching agents that
    // are gone, or agents asking a coordinator that is gone. Neither is a state
    // anybody asks for, so the row kills the run as one thing.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));

    let mut pids = kill_dialog(&state).pids.clone();
    pids.sort_unstable();
    assert_eq!(
        pids,
        vec![101, 202],
        "the kill list is not exactly the coordinator and its agent"
    );
}

#[test]
fn the_kill_dialog_names_the_coordinator_separately() {
    // Killing the coordinator is the part with a consequence the agents do not
    // have: the run stops being answered. The confirmation should say so
    // before you press Enter, not after.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));

    let label = &kill_dialog(&state).label;
    assert!(
        label.contains("coordinator"),
        "the dialog does not mention the coordinator: {label:?}"
    );
    assert!(
        label.contains("x/alpha"),
        "the dialog does not say which run: {label:?}"
    );
}

#[test]
fn a_run_with_no_coordinator_still_kills_its_agents() {
    // And says there is no coordinator, so the count is not read as one.
    let mut state = with_a_run(false);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));

    let confirm = kill_dialog(&state);
    assert!(!confirm.pids.is_empty(), "the agents were not collected");
    assert!(
        confirm.label.contains("no coordinator"),
        "the dialog does not say the coordinator is missing: {:?}",
        confirm.label
    );
}

#[test]
fn killing_a_run_does_not_touch_sessions_outside_it() {
    // The whole risk of a bulk kill. A session in the same folder that the run
    // does not own must not be in the list.
    let mut state = with_a_run(true);
    let mut bystander = session("other", "/Users/x/dev/alpha", SessionStatus::Working);
    bystander.pids = vec![9999];
    let mut all: Vec<crate::types::Session> = state.sessions().cloned().collect();
    all.push(bystander);
    with_sessions(&mut state, all);
    state.take_actions();

    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));

    assert!(
        !kill_dialog(&state).pids.contains(&9999),
        "a session outside the run was included: {:?}",
        kill_dialog(&state).pids
    );
}

#[test]
fn confirming_a_run_kill_also_ends_the_run() {
    // The sessions are going. A run row left behind with nothing under it is
    // something you can neither act on nor get rid of, and the sessions tab
    // should go back to looking the way it does with no run open.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));
    press(&mut state, KeyCode::Enter);

    assert!(
        state.board.runs.is_empty(),
        "the run survived its own kill: {:?}",
        state.board.runs
    );
    assert!(
        state
            .take_actions()
            .iter()
            .any(|action| matches!(action, Action::Kill { .. })),
        "the processes were not killed"
    );
}

#[test]
fn cancelling_a_run_kill_leaves_the_run_alone() {
    // Esc must not end the run. Confirming is the decision, not opening.
    let mut state = with_a_run(true);
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Char('x'));
    press(&mut state, KeyCode::Esc);

    assert_eq!(state.board.runs.len(), 1, "Esc ended the run");
}

#[test]
fn killing_one_session_does_not_end_its_run() {
    // `x` on an AGENT row kills that session only. Ending the run there would
    // take out the coordinator and the six agents you did not name.
    let mut state = with_a_run(true);
    // Open the run so its agent rows exist.
    on_the_run_row(&mut state);
    press(&mut state, KeyCode::Right);
    let snapshot = tree_snapshot(&state);
    let index = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("ra:"))
        .expect("an agent row");
    state.tree_sel.set(&snapshot.keys, index);
    press(&mut state, KeyCode::Char('x'));
    press(&mut state, KeyCode::Enter);

    assert_eq!(
        state.board.runs.len(),
        1,
        "killing one agent ended the whole run"
    );
}

// --- an empty scan must not erase the tree -----------------------------------

#[test]
fn an_empty_scan_keeps_the_previous_sessions() {
    // A scan that reads nothing is not evidence that nothing is running. Deep
    // in swap, `lsof` took seconds against a 5s timeout and processes whose cwd
    // could not be read were dropped, so a slow moment produced "no sessions"
    // and the tree blanked while every agent was fine.
    let mut state = temp_state().1;
    with_sessions(&mut state, three_sessions());
    state.take_actions();
    let before = state.by_project.clone();

    state.apply_sessions(group_sessions(Vec::new()));

    assert_eq!(state.by_project, before, "an empty scan erased the tree");
    assert!(state.feed_went_quiet, "nothing says the list is stale");
}

#[test]
fn a_real_session_list_clears_the_quiet_flag() {
    let mut state = temp_state().1;
    with_sessions(&mut state, three_sessions());
    state.apply_sessions(group_sessions(Vec::new()));
    assert!(state.feed_went_quiet);

    with_sessions(&mut state, three_sessions());
    assert!(
        !state.feed_went_quiet,
        "the stale notice outlived the outage"
    );
}

#[test]
fn an_empty_scan_on_an_empty_tree_is_accepted() {
    // Starting with nothing running is a real state, and must not be reported
    // as a failed read.
    let mut state = temp_state().1;
    state.apply_sessions(group_sessions(Vec::new()));
    assert!(state.by_project.is_empty());
    assert!(!state.feed_went_quiet);
}
