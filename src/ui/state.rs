//! Everything the dashboard draws from, owned by one value.
//!
//! The Node app kept this in `src/state.js` as a module-global mutable
//! singleton that both the engine and the TUI wrote to — the brief calls it the
//! biggest structural wart in the codebase. Here it is a plain struct threaded
//! through the draw and key handlers, so two dashboards in one test process
//! cannot see each other's state.
//!
//! Nothing in this file performs I/O. Work that would block — spawning a
//! terminal, killing a process, opening a transcript — is *enqueued* as an
//! [`Action`] and run elsewhere (brief §10 mandate #9), which is also what makes
//! the key handlers testable without a machine to act on.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::config::ConfigHandle;
use crate::paths::Paths;
use crate::types::{ConversationMessage, DefaultView, Notification, NotificationStatus};
use crate::ui::board::slice::{BoardSlice, BoardUpdate};
use crate::ui::conversation::ConversationMeta;
use crate::ui::dialogs::Dialog;
use crate::ui::tree::SessionsByProject;

mod action;

pub use action::Action;

/// How many notifications the feed keeps. A `VecDeque` rather than Node's
/// `unshift` + `length = 200` (brief §10 mandate #12).
pub const MAX_NOTIFICATIONS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Sessions,
    Board,
    Deploy,
}

impl View {
    pub fn next(self) -> View {
        match self {
            View::Sessions => View::Board,
            View::Board => View::Deploy,
            View::Deploy => View::Sessions,
        }
    }

    pub fn from_default(view: DefaultView) -> View {
        match view {
            DefaultView::Sessions => View::Sessions,
            DefaultView::Board => View::Board,
            DefaultView::Deploy => View::Deploy,
        }
    }
}

/// Which pane has the keyboard. Shift-Tab swaps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Conversation,
}

/// Why the loop is stopping. `q` detaches and leaves the daemon working —
/// that is the whole point of having one — while `Q` (confirmed) stops it too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    Detach,
    ShutdownAll,
}

/// Plan-usage readout for the header.
///
/// The reading itself is [`crate::usage::UsageSnapshot`] — the header wants
/// exactly what the `/usage` parser produces, and a second shape here would be
/// one more thing to keep in step. Aliased rather than re-declared so the
/// header's `Option<&UsageReadout>` slot reads the way it always has.
pub use crate::usage::UsageSnapshot as UsageReadout;

/// Header counters. Derived from the session list on every feed update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub total_sessions: usize,
    pub total_projects: usize,
}

/// A view's cursor: an identity key that survives a reshuffle, plus the index it
/// falls back to when the keyed row disappears entirely.
///
/// This is why a session finishing three rows above the cursor does not move the
/// cursor. The Node app pinned the same behaviour per view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    key: Option<String>,
    index: usize,
}

impl Selection {
    /// Where the cursor actually is, given the rows as they are right now.
    pub fn resolve(&self, keys: &[String]) -> usize {
        if keys.is_empty() {
            return 0;
        }
        if let Some(stored) = &self.key {
            if let Some(found) = keys.iter().position(|k| k == stored) {
                return found;
            }
        }
        self.index.min(keys.len() - 1)
    }

    pub fn set(&mut self, keys: &[String], index: usize) {
        if keys.is_empty() {
            self.key = None;
            self.index = 0;
            return;
        }
        let index = index.min(keys.len() - 1);
        self.key = Some(keys[index].clone());
        self.index = index;
    }

    pub fn move_by(&mut self, keys: &[String], delta: isize) {
        if keys.is_empty() {
            return;
        }
        let current = self.resolve(keys) as isize;
        let next = (current + delta).clamp(0, keys.len() as isize - 1);
        self.set(keys, next as usize);
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

/// The conversation pane's own state. Scroll position lives here rather than in
/// the widget so it survives a redraw and a feed update.
#[derive(Debug, Clone, Default)]
pub struct ConversationView {
    pub messages: Vec<ConversationMessage>,
    pub meta: Option<ConversationMeta>,
    pub scroll_top: usize,
    /// Pinned to the bottom. `G` re-sticks; scrolling up releases it, so a busy
    /// session does not yank the pane away from what you were reading.
    pub stick: bool,
    /// Rows the pane held last time it was drawn, and how many wrapped lines it
    /// had to show. Page-scrolling needs both, and the pane only learns them at
    /// draw time — re-wrapping a whole transcript inside a key handler just to
    /// answer "how far is a page" would be the expensive way to find out.
    pub page_height: usize,
    pub total_lines: usize,
}

pub struct AppState {
    pub paths: Paths,
    pub config: ConfigHandle,
    pub view: View,
    pub focus: Pane,
    pub by_project: SessionsByProject,
    /// Group path → the folders discovered inside it.
    pub discovered_dirs: BTreeMap<String, Vec<String>>,
    pub expanded_projects: HashSet<String>,
    seen_projects: HashSet<String>,
    initialised: bool,
    pub tree_sel: Selection,
    pub board_sel: Selection,
    pub deploy_sel: Selection,
    pub list_scroll: usize,
    pub conv: ConversationView,
    pub selected_session_id: Option<String>,
    pub selected_session_file: Option<PathBuf>,
    pub notifications: VecDeque<Notification>,
    /// The board tab's own state, kept in its own type so neither this file nor
    /// [`crate::ui::board`] becomes the god object Node's `state.js` was.
    pub board: BoardSlice,
    pub dialog: Option<Dialog>,
    /// Transient message shown in the right-hand pane, cleared on the next
    /// selection or view change.
    pub flash: Option<String>,
    pub usage: Option<UsageReadout>,
    /// Short `HEAD`s per QA worktree, shared with the action worker. Read here
    /// while labelling the QA menu row; filled only by the worker, so no
    /// render path ever spawns `git` (brief §10 mandate #9).
    pub qa_heads: std::sync::Arc<crate::qaden::HeadCache>,
    pub stats: Stats,
    pub quit: Option<Quit>,
    /// Set by anything that changes what is on screen. The loop draws when it
    /// is set and sleeps otherwise — the ratatui equivalent of `store.js`'s
    /// signature hashing, without hashing anything (brief §10 mandate #7).
    pub dirty: bool,
    pending: VecDeque<Action>,
}

impl AppState {
    pub fn new(paths: Paths, config: ConfigHandle) -> Self {
        let view = View::from_default(config.default_view());
        AppState {
            paths,
            config,
            view,
            focus: Pane::Tree,
            by_project: SessionsByProject::new(),
            discovered_dirs: BTreeMap::new(),
            expanded_projects: HashSet::new(),
            seen_projects: HashSet::new(),
            initialised: false,
            tree_sel: Selection::default(),
            board_sel: Selection::default(),
            deploy_sel: Selection::default(),
            list_scroll: 0,
            conv: ConversationView::default(),
            selected_session_id: None,
            selected_session_file: None,
            notifications: VecDeque::new(),
            board: BoardSlice::default(),
            dialog: None,
            flash: None,
            usage: None,
            qa_heads: std::sync::Arc::new(crate::qaden::HeadCache::new()),
            stats: Stats::default(),
            quit: None,
            dirty: true,
            pending: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, action: Action) {
        self.pending.push_back(action);
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        self.pending.drain(..).collect()
    }

    /// Visible only to tests and the loop; a caller that wants to know what is
    /// queued without consuming it.
    pub fn pending_actions(&self) -> &VecDeque<Action> {
        &self.pending
    }

    pub fn flash(&mut self, message: impl Into<String>) {
        self.flash = Some(message.into());
        self.dirty = true;
    }

    pub fn selection_for(&mut self, view: View) -> &mut Selection {
        match view {
            View::Sessions => &mut self.tree_sel,
            View::Board => &mut self.board_sel,
            View::Deploy => &mut self.deploy_sel,
        }
    }

    /// Replace the session list. A project seen for the first time opens itself,
    /// so a launch into a quiet folder is visible without pressing anything —
    /// but a project the user has since collapsed stays collapsed.
    pub fn apply_sessions(&mut self, by_project: SessionsByProject) {
        let names: Vec<String> = by_project.keys().cloned().collect();
        if !self.initialised {
            self.expanded_projects.extend(names.iter().cloned());
            self.initialised = true;
        } else {
            for name in &names {
                if !self.seen_projects.contains(name) {
                    self.expanded_projects.insert(name.clone());
                }
            }
        }
        self.seen_projects = names.iter().cloned().collect();
        self.stats = Stats {
            total_sessions: by_project.values().map(Vec::len).sum(),
            total_projects: by_project.len(),
        };
        self.by_project = by_project;
        self.dirty = true;
    }

    /// Fold a plan-usage reading in.
    ///
    /// A failed check keeps the previous numbers and marks them stale rather
    /// than blanking the readout — see [`crate::usage::UsageSnapshot::merge_over`].
    pub fn apply_usage(&mut self, usage: crate::usage::UsageSnapshot) {
        self.usage = Some(usage.merge_over(self.usage.as_ref()));
        self.dirty = true;
    }

    /// Fold a board fetch in. Called from both feeds — the daemon's `board`
    /// event and the in-process fetch — so the two can never diverge.
    pub fn apply_board(&mut self, update: BoardUpdate) {
        self.board.apply(update);
        self.dirty = true;
    }

    /// The notification with this id, for the feed rows and the menu.
    pub fn notification(&self, id: &str) -> Option<&Notification> {
        self.notifications.iter().find(|n| n.id == id)
    }

    /// Change a notification's status locally, for immediate feedback.
    ///
    /// The list's real owner is the engine, which persists it and serves it to
    /// every client on connect — mutating only the local copy is why a resolved
    /// notification used to come back on the next reconnect. The caller also
    /// enqueues [`Action::Notifications`] so the write reaches it.
    pub fn set_notification_status(&mut self, id: &str, status: NotificationStatus) {
        if let Some(notification) = self.notifications.iter_mut().find(|n| n.id == id) {
            notification.status = status;
            self.dirty = true;
        }
    }

    pub fn dismiss_notification(&mut self, id: &str) {
        self.notifications.retain(|n| n.id != id);
        self.dirty = true;
    }

    /// A new notification rings, then joins the feed.
    ///
    /// The sound is enqueued rather than played here: the draw thread starts no
    /// processes (brief §10 mandate #9), and `afplay` is a process like any
    /// other.
    pub fn push_notification(&mut self, notification: Notification) {
        self.enqueue(Action::Sound(notification.level));
        self.notifications.push_front(notification);
        while self.notifications.len() > MAX_NOTIFICATIONS {
            self.notifications.pop_back();
        }
        self.dirty = true;
    }

    /// Every live session, flattened. Built on demand rather than kept as a
    /// second copy — Node's `Object.values().flat()` appeared in eight places.
    /// `+ Clone` because the task-session resolution walks the list more than
    /// once (by transcript task id, then by recorded link) and re-borrowing the
    /// state between passes would fight the borrow checker for no gain.
    pub fn sessions(&self) -> impl Iterator<Item = &crate::types::Session> + Clone {
        self.by_project.values().flatten()
    }

    pub fn find_session(&self, session_id: &str) -> Option<&crate::types::Session> {
        self.sessions().find(|s| s.session_id == session_id)
    }

    /// One page of the conversation pane, as it was last drawn.
    pub fn conv_page(&self) -> usize {
        self.conv.page_height.max(1)
    }

    pub fn conv_max_scroll(&self) -> usize {
        self.conv.total_lines.saturating_sub(self.conv.page_height)
    }
}
