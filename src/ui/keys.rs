//! Key routing: global keys, then the focused pane's.
//!
//! Ported from the `useInput` handlers in the Node app's `src/tui/App.js`.
//! Everything here is a pure function of `(state, key)` — nothing spawns, reads
//! a file or draws — so a whole key sequence can be replayed in a test and the
//! resulting state and action queue inspected.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use crate::term::SessionRef;
use crate::ui::board::handle_board;
use crate::ui::dialogs::{
    AddGroup, Dialog, DialogCtx, KillConfirm, LogViewer, PurgeConfirm, Rename, Search,
    SettingsDialog, ShutdownConfirm,
};
use crate::ui::state::{Action, AppState, Pane, Quit, View};
use crate::ui::tree::{build_grouped_tree, SelectedRow, TreeItem};

mod dialog;

use dialog::apply_dialog_outcome;
pub use dialog::go_to_session;

/// Carry out a dialog outcome without a dialog being open — how a test drives
/// the half of a flow that lives in the dispatcher.
#[cfg(test)]
pub(crate) fn apply_outcome_for_test(
    state: &mut AppState,
    outcome: crate::ui::dialogs::DialogOutcome,
) {
    let placeholder = Dialog::Shutdown(ShutdownConfirm::default());
    apply_dialog_outcome(state, placeholder, outcome);
}

/// A read-only look at the sessions tree as it stands right now.
pub struct TreeSnapshot {
    pub keys: Vec<String>,
    pub selected: usize,
    pub row: Option<SelectedRow>,
    /// The directory `n` would launch into, if this row names one.
    pub dir: Option<String>,
}

pub fn tree_snapshot(state: &AppState) -> TreeSnapshot {
    let groups = state.config.groups();
    let items: Vec<TreeItem<'_>> = build_grouped_tree(
        &state.by_project,
        &state.expanded_projects,
        &groups,
        &state.discovered_dirs,
    );
    let keys: Vec<String> = items.iter().map(TreeItem::key).collect();
    let selected = state.tree_sel.resolve(&keys);
    let item = items.get(selected);
    TreeSnapshot {
        row: item.map(TreeItem::to_selected),
        dir: item.and_then(|i| i.dir_path(&state.by_project)),
        keys,
        selected,
    }
}

/// Route one key. `area` is the body rectangle, which the file viewer needs to
/// size its pages.
pub fn handle_key(state: &mut AppState, key: KeyEvent, area: Rect) {
    // crossterm reports press *and* release on some terminals; acting on both
    // would double every keystroke.
    if key.kind == KeyEventKind::Release {
        return;
    }
    state.dirty = true;

    // A modal swallows input first: this is what keeps `q` from quitting the
    // dashboard out from under a confirmation dialog.
    if let Some(mut dialog) = state.dialog.take() {
        let outcome = {
            let mut ctx = DialogCtx {
                config: &mut state.config,
            };
            dialog.handle_key(key, area, &mut ctx)
        };
        apply_dialog_outcome(state, dialog, outcome);
        return;
    }

    if handle_global(state, key) {
        return;
    }
    // The right-hand pane's keys, on EVERY view — Node routed them before the
    // per-view map (`App.js:251-253`). Without this a task description or a
    // deploy log longer than the pane could not be read past its first page.
    if state.focus == Pane::Conversation && handle_conversation(state, key) {
        return;
    }
    match state.view {
        View::Sessions => handle_sessions(state, key),
        View::Board => handle_board(state, key),
        View::Deploy => crate::ui::deploy::handle_deploy(state, key),
    }
}

/// Keys that mean the same thing everywhere. Returns true when it took the key.
fn handle_global(state: &mut AppState, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Detach. In remote mode the daemon keeps running — that is the point of
        // having one — so this closes the dashboard and nothing else.
        KeyCode::Char('c') if ctrl => {
            state.quit = Some(Quit::Detach);
            true
        }
        KeyCode::Char('q') => {
            state.quit = Some(Quit::Detach);
            true
        }
        // Shift-Q stops the daemon too, which can kill work — so it confirms.
        KeyCode::Char('Q') => {
            // The deploys it names are the ones actually running: stopping the
            // daemon kills its children, and saying so is the whole reason
            // this confirmation exists.
            state.dialog = Some(Dialog::Shutdown(
                ShutdownConfirm::from_sessions(state.sessions())
                    .with_deploys(state.deploy.running_deploys()),
            ));
            true
        }
        KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            set_view(state, state.view.next());
            true
        }
        KeyCode::BackTab | KeyCode::Tab => {
            state.focus = match state.focus {
                Pane::Tree => Pane::Conversation,
                Pane::Conversation => Pane::Tree,
            };
            true
        }
        // Plan usage is itself a request against the quota it reports, so it
        // is manual unless a `usage.intervalMinutes` is configured.
        KeyCode::Char('u') => {
            refresh_usage(state);
            true
        }
        // On the Deploy tab `L` is the running deploy's output; lowercase `l`
        // stays the standup log everywhere. The Node original special-cases it
        // in exactly this order, before the global arm below.
        KeyCode::Char('L') if state.view == View::Deploy => false,
        KeyCode::Char('l') | KeyCode::Char('L') => {
            // A connection of its own: the day list is the union of the
            // markdown files and the database, and the database knows about
            // days whose file was moved or deleted.
            let db = crate::db::Db::open(&state.paths);
            state.dialog = Some(Dialog::LogViewer(Box::new(LogViewer::open(
                &state.paths,
                Some(&db),
                &crate::dailylog::ymd(chrono::Local::now()),
            ))));
            state.dirty = true;
            true
        }
        KeyCode::Char('r') if !renames_instead(state) => {
            state.enqueue(Action::Refresh);
            // On the board and deploy tabs `r` is also the retry the error
            // message asks for, so it refetches rather than only rescanning
            // processes. On the Deploy tab it is the ONLY thing that ever
            // refetches: every load spends a GitLab call per open MR.
            match state.view {
                View::Board => crate::ui::board::keys::refresh(state),
                View::Deploy => crate::ui::deploy::keys::refresh(state),
                View::Sessions => {}
            }
            true
        }
        _ => false,
    }
}

/// `u` — take a plan-usage reading, wherever the check actually lives.
///
/// Queued as an [`Action`] rather than run here: the check shells out to
/// `claude`, which takes seconds. The loop hands it to the daemon when there is
/// one (see [`crate::ui::feed::SessionFeed::refresh_usage`]) so several
/// dashboards do not each spend the quota.
pub fn refresh_usage(state: &mut AppState) {
    if !state.config.usage().enabled {
        state.flash("Plan usage checks are off (\"usage\": {\"enabled\": false}).");
        return;
    }
    state.enqueue(Action::RefreshUsage);
    state.flash("Checking plan usage… (this spends a request against it)");
}

/// `X` — the purge confirmation, primed with every stage the board already
/// knows and a lookup for the rest.
fn open_purge(state: &mut AppState) {
    let targets: Vec<crate::purge::PurgeTarget> = state
        .sessions()
        .map(crate::purge::PurgeTarget::from_session)
        .collect();
    let known = state.board.stages();
    let dialog = PurgeConfirm::new(targets, known);
    let missing = dialog.missing_stages();
    if !missing.is_empty() {
        state.enqueue(Action::FetchTaskStages { task_ids: missing });
    }
    state.dialog = Some(Dialog::PurgeConfirm(dialog));
    state.dirty = true;
}

/// `r` is refresh everywhere except on a session row, where it renames — the one
/// place the Node app overloaded a global key.
fn renames_instead(state: &AppState) -> bool {
    if state.view != View::Sessions || state.focus != Pane::Tree {
        return false;
    }
    matches!(tree_snapshot(state).row, Some(SelectedRow::Session { .. }))
}

fn set_view(state: &mut AppState, view: View) {
    state.view = view;
    state.focus = Pane::Tree;
    state.list_scroll = 0;
    state.flash = None;
    // Arriving at an empty board fetches it, so the tab fills in rather than
    // waiting out the 45-second poll.
    if view == View::Board && state.board.board.is_none() && !state.board.loading {
        state.board.loading = true;
        crate::ui::board::keys::refresh(state);
    }
}

fn handle_sessions(state: &mut AppState, key: KeyEvent) {
    let snapshot = tree_snapshot(state);
    // `o` is the one key the conversation pane does not shadow: Node handled it
    // view-wide, before the focus check, and fell back to the selected session
    // when the cursor was not on a session row (`App.js:242-247`). Reaching the
    // terminal is how a permission prompt gets answered, so it must work from
    // wherever you noticed the prompt.
    if key.code == KeyCode::Char('o') {
        focus_selected_terminal(state, &snapshot);
        return;
    }
    if state.focus != Pane::Tree {
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.tree_sel.move_by(&snapshot.keys, -1),
        KeyCode::Down | KeyCode::Char('j') => state.tree_sel.move_by(&snapshot.keys, 1),
        KeyCode::Enter => on_select(state, &snapshot),
        KeyCode::Right => on_expand(state, &snapshot, true),
        KeyCode::Left => on_expand(state, &snapshot, false),
        _ => panel_key(state, key, &snapshot),
    }
}

/// Raise the terminal of the session under the cursor, or — when the cursor is
/// somewhere else — of the session the conversation pane is showing.
fn focus_selected_terminal(state: &mut AppState, snapshot: &TreeSnapshot) {
    let id = match &snapshot.row {
        Some(SelectedRow::Session { session_id, .. }) => Some(session_id.clone()),
        _ => state.selected_session_id.clone(),
    };
    let Some(session) = id.and_then(|id| state.find_session(&id)) else {
        return;
    };
    let reference = SessionRef::from_session(session);
    state.enqueue(Action::FocusTerminal(Box::new(reference)));
}

fn panel_key(state: &mut AppState, key: KeyEvent, snapshot: &TreeSnapshot) {
    match key.code {
        KeyCode::Char('n') => {
            if let Some(dir) = snapshot.dir.clone() {
                state.enqueue(Action::LaunchSession { cwd: dir });
            }
        }
        KeyCode::Char('x') => {
            state.dialog = Some(Dialog::Kill(KillConfirm::for_row(
                snapshot.row.as_ref(),
                &state.by_project,
                &state.config,
            )));
        }
        // X is the bulk form of x: it closes every session whose task has
        // finished, and nothing else. What "finished" means is
        // [`crate::purge`]; the dialog only confirms it.
        KeyCode::Char('X') => open_purge(state),
        KeyCode::Char('r') => {
            if let Some(SelectedRow::Session { session_id, .. }) = &snapshot.row {
                state.dialog = Some(Dialog::Rename(Rename::new(
                    session_id.clone(),
                    &state.config,
                )));
            }
        }
        KeyCode::Char('a') => state.dialog = Some(Dialog::AddGroup(AddGroup::new())),
        KeyCode::Char('d') => {
            if let Some(SelectedRow::Separator {
                group_index: Some(index),
                ..
            }) = snapshot.row
            {
                let _ = state.config.remove_group(index);
                state.enqueue(Action::Refresh);
            }
        }
        KeyCode::Char('s') => state.dialog = Some(Dialog::Settings(SettingsDialog::default())),
        _ => {}
    }
}

fn on_select(state: &mut AppState, snapshot: &TreeSnapshot) {
    match &snapshot.row {
        Some(SelectedRow::Project { name }) => {
            if !state.expanded_projects.remove(name) {
                state.expanded_projects.insert(name.clone());
            }
        }
        Some(SelectedRow::Session { session_id, .. }) => select_session(state, session_id),
        _ => {}
    }
}

fn on_expand(state: &mut AppState, snapshot: &TreeSnapshot, expand: bool) {
    match &snapshot.row {
        Some(SelectedRow::Project { name }) => {
            if expand {
                state.expanded_projects.insert(name.clone());
            } else {
                state.expanded_projects.remove(name);
            }
        }
        Some(SelectedRow::Session { session_id, .. }) => {
            if expand {
                select_session(state, session_id);
            } else {
                // Left on a session collapses its project, which is how you get
                // back out of a long list without scrolling to the header.
                let project = state
                    .find_session(session_id)
                    .map(|s| crate::util::project_name(&s.cwd));
                if let Some(project) = project {
                    state.expanded_projects.remove(&project);
                }
            }
        }
        _ => {}
    }
}

fn select_session(state: &mut AppState, session_id: &str) {
    let Some(session) = state.find_session(session_id) else {
        return;
    };
    let file = session.session_file.clone();
    state.selected_session_id = Some(session_id.to_string());
    state.selected_session_file = file.clone();
    state.conv.stick = true;
    state.conv.scroll_top = 0;
    state.flash = None;
    state.enqueue(Action::SelectSession {
        session_id: session_id.to_string(),
        session_file: file,
    });
}

/// Conversation-pane keys. Returns true when the key was consumed.
fn handle_conversation(state: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('t') => {
            let mut chat = state.config.chat().clone();
            chat.show_timestamps = !chat.show_timestamps;
            let _ = state.config.save_chat_config(chat);
            true
        }
        KeyCode::Char('f') => {
            let mut chat = state.config.chat().clone();
            let order = ["all", "user", "assistant"];
            let at = order
                .iter()
                .position(|f| *f == chat.message_filter)
                .unwrap_or(0);
            chat.message_filter = order[(at + 1) % order.len()].to_string();
            let _ = state.config.save_chat_config(chat);
            true
        }
        KeyCode::Char('/') => {
            state.dialog = Some(Dialog::Search(Search::new(&state.config)));
            true
        }
        KeyCode::Char('s') => {
            state.dialog = Some(Dialog::Settings(SettingsDialog::default()));
            true
        }
        // Scroll keys work against the LAST measured page height: the pane knows
        // its own size only at draw time, and storing it is cheaper than
        // re-wrapping the whole transcript here to find out.
        KeyCode::Up | KeyCode::Char('k') => {
            scroll_by(state, -1);
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            scroll_by(state, 1);
            true
        }
        KeyCode::PageUp => {
            scroll_by(state, -(state.conv_page() as isize));
            true
        }
        KeyCode::PageDown | KeyCode::Char(' ') => {
            scroll_by(state, state.conv_page() as isize);
            true
        }
        KeyCode::Char('g') => {
            state.conv.stick = false;
            state.conv.scroll_top = 0;
            true
        }
        // G re-sticks, so a session that is still writing keeps the pane at the
        // newest line rather than leaving you at a fixed offset.
        KeyCode::Char('G') => {
            state.conv.stick = true;
            true
        }
        _ => false,
    }
}

fn scroll_by(state: &mut AppState, delta: isize) {
    let max = state.conv_max_scroll();
    let next = (state.conv.scroll_top as isize + delta).clamp(0, max as isize) as usize;
    state.conv.scroll_top = next;
    // Scrolling to the very bottom re-sticks; scrolling up releases.
    state.conv.stick = delta > 0 && next >= max;
}
