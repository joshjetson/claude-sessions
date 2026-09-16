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
use crate::term::SessionRef;
use crate::types::{ConversationMessage, DefaultView, Notification};
use crate::ui::conversation::ConversationMeta;
use crate::ui::dialogs::Dialog;
use crate::ui::tree::SessionsByProject;

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

/// Work the UI thread refuses to do itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask the feed for a fresh scan now.
    Refresh,
    /// Refresh at 0/400/1000ms. SIGTERM is not instant — the process lingers in
    /// `ps` for a moment — so Node polled three times after a kill, and a single
    /// refresh here would redraw the row it just killed.
    RefreshBurst,
    /// Read this transcript into the conversation pane.
    SelectSession {
        session_id: String,
        session_file: Option<PathBuf>,
    },
    FocusTerminal(Box<SessionRef>),
    LaunchSession {
        cwd: String,
    },
    Kill {
        pids: Vec<u32>,
        label: String,
    },
    /// Hand the terminal to `$EDITOR` and take it back — Node's `suspendUI`.
    OpenEditor {
        path: String,
        line: u32,
    },
}

/// Plan-usage readout for the header.
///
/// Phase 11 ports `usage.js` and fills this in; the slot exists now so adding it
/// later changes no layout. The header takes `Option<&UsageReadout>` and draws
/// nothing at all while it is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageReadout {
    pub session_pct: Option<u8>,
    pub week_pct: Option<u8>,
    pub fable_pct: Option<u8>,
}

impl UsageReadout {
    /// The widest form that fits `available` columns, down to bare percentages —
    /// so the readout never collides with the centred title.
    pub fn format(&self, available: usize) -> Option<String> {
        let parts: Vec<(char, u8)> = [
            ('s', self.session_pct),
            ('w', self.week_pct),
            ('f', self.fable_pct),
        ]
        .into_iter()
        .filter_map(|(tag, pct)| pct.map(|p| (tag, p)))
        .collect();
        if parts.is_empty() {
            return None;
        }
        let long = parts
            .iter()
            .map(|(tag, pct)| {
                let name = match tag {
                    's' => "session",
                    'w' => "week",
                    _ => "fable",
                };
                format!("{name} {pct}%")
            })
            .collect::<Vec<_>>()
            .join("  ");
        let short = parts
            .iter()
            .map(|(tag, pct)| format!("{tag}{pct}%"))
            .collect::<Vec<_>>()
            .join(" ");
        [format!(" {long} "), format!(" {short} ")]
            .into_iter()
            .find(|candidate| candidate.chars().count() <= available)
    }
}

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
    pub dialog: Option<Dialog>,
    /// Transient message shown in the right-hand pane, cleared on the next
    /// selection or view change.
    pub flash: Option<String>,
    pub usage: Option<UsageReadout>,
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
            dialog: None,
            flash: None,
            usage: None,
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

    pub fn push_notification(&mut self, notification: Notification) {
        self.notifications.push_front(notification);
        while self.notifications.len() > MAX_NOTIFICATIONS {
            self.notifications.pop_back();
        }
        self.dirty = true;
    }

    /// Every live session, flattened. Built on demand rather than kept as a
    /// second copy — Node's `Object.values().flat()` appeared in eight places.
    pub fn sessions(&self) -> impl Iterator<Item = &crate::types::Session> {
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
