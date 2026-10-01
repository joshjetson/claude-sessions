//! Every key the dashboard binds, in one table.
//!
//! The table drives three things that used to be written out by hand and
//! drifted apart: the status bar, the `?` help, and the Keys page in settings.
//! It also says which role each key is for, so a reviewer is not offered the
//! keys that start development work, and a developer is not offered the QA
//! run keys. The PM role keeps every key.
//!
//! The handlers still match on each key's default character. A key the
//! person remapped is translated back to that default before a handler sees
//! it (see [`Keymap::route`]), so remapping needs no change in any handler.
//! Navigation keys (arrows, Enter, Tab, `j`/`k`) and `q`, `Q`, `?` cannot be
//! remapped: they are how you get back out of a mistake.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::types::UserRole;
use crate::ui::state::{Pane, View};

/// Where a key applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every view.
    Global,
    /// The sessions list.
    Sessions,
    /// The right-hand pane, on every view, while it has the focus.
    Conversation,
    Board,
    Deploy,
}

impl Scope {
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Everywhere",
            Scope::Sessions => "Sessions list",
            Scope::Conversation => "Right-hand pane",
            Scope::Board => "Board",
            Scope::Deploy => "Deploy",
        }
    }

    /// Whether one key could mean a binding in each scope at the same moment.
    /// The right-hand pane takes its keys before every view, so it overlaps
    /// them all.
    pub fn overlaps(self, other: Scope) -> bool {
        self == other
            || matches!(self, Scope::Global | Scope::Conversation)
            || matches!(other, Scope::Global | Scope::Conversation)
    }
}

/// The roles a key is offered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    dev: bool,
    qa: bool,
    pm: bool,
}

/// Every role.
pub const ALL: Roles = Roles {
    dev: true,
    qa: true,
    pm: true,
};
/// Development work: not the QA role.
pub const NOT_QA: Roles = Roles {
    dev: true,
    qa: false,
    pm: true,
};
/// QA runs: not the developer role.
pub const NOT_DEV: Roles = Roles {
    dev: false,
    qa: true,
    pm: true,
};

impl Roles {
    pub fn has(self, role: UserRole) -> bool {
        match role {
            UserRole::Dev => self.dev,
            UserRole::Qa => self.qa,
            UserRole::Pm => self.pm,
        }
    }
}

/// A key as the table writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A key with a name, such as Enter. Shown, never remapped.
    Named(&'static str),
}

/// One binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Stable, and the name in the config's `keys` block: `board.start`.
    pub id: &'static str,
    pub scope: Scope,
    /// The default key. The handlers match on it.
    pub key: Key,
    /// The status bar's word for it. Empty for a key the bar never shows.
    pub hint: &'static str,
    /// The `?` help's and the Keys page's line for it.
    pub help: &'static str,
    pub roles: Roles,
    /// The status bar fills in this order, lowest first, until the line is
    /// full. 0 is never on the bar.
    pub rank: u8,
    /// Cannot be remapped.
    pub fixed: bool,
    /// Said after "not offered to your role", when there is somewhere better
    /// to go.
    pub note: &'static str,
}

const fn bind(
    id: &'static str,
    scope: Scope,
    key: char,
    hint: &'static str,
    help: &'static str,
    rank: u8,
) -> Binding {
    Binding {
        id,
        scope,
        key: Key::Char(key),
        hint,
        help,
        roles: ALL,
        rank,
        fixed: false,
        note: "",
    }
}

const fn named(
    id: &'static str,
    scope: Scope,
    key: &'static str,
    hint: &'static str,
    help: &'static str,
    rank: u8,
) -> Binding {
    Binding {
        key: Key::Named(key),
        fixed: true,
        ..bind(id, scope, ' ', hint, help, rank)
    }
}

impl Binding {
    const fn only(self, roles: Roles) -> Binding {
        Binding { roles, ..self }
    }

    const fn fixed(self) -> Binding {
        Binding {
            fixed: true,
            ..self
        }
    }

    const fn note(self, note: &'static str) -> Binding {
        Binding { note, ..self }
    }

    /// The default character, for a key that has one.
    pub fn default_char(&self) -> Option<char> {
        match self.key {
            Key::Char(ch) => Some(ch),
            Key::Named(_) => None,
        }
    }
}

const DEV_WORK: &str = "Press Enter on the task and pick QA to start a pass.";

/// Every binding, in the order the help lists them.
pub const BINDINGS: &[Binding] = &[
    // --- everywhere ---------------------------------------------------------
    named(
        "global.view",
        Scope::Global,
        "Tab",
        "view",
        "Next view: Sessions, Board, Deploy",
        30,
    ),
    named(
        "global.panel",
        Scope::Global,
        "S-Tab",
        "panel",
        "Switch between the list and the right-hand pane",
        31,
    ),
    bind(
        "global.settings",
        Scope::Global,
        ',',
        "settings",
        "Settings",
        20,
    ),
    bind(
        "global.refresh",
        Scope::Global,
        'r',
        "refresh",
        "Refresh: rescan, and refetch the board or deploys",
        15,
    ),
    bind(
        "global.usage",
        Scope::Global,
        'u',
        "usage",
        "Check plan usage (spends a request)",
        40,
    ),
    bind("global.log", Scope::Global, 'l', "log", "Standup log", 41),
    bind(
        "global.help",
        Scope::Global,
        '?',
        "keys",
        "Every key for this view",
        0,
    )
    .fixed(),
    bind(
        "global.quit",
        Scope::Global,
        'q',
        "quit",
        "Close the dashboard. The daemon keeps running",
        0,
    )
    .fixed(),
    bind(
        "global.stop",
        Scope::Global,
        'Q',
        "stop all",
        "Stop the daemon too, after a confirmation",
        50,
    )
    .fixed(),
    // --- sessions list ------------------------------------------------------
    named("sessions.move", Scope::Sessions, "↑↓ j k", "", "Move", 0),
    named(
        "sessions.select",
        Scope::Sessions,
        "Enter",
        "select",
        "Open the conversation, or expand the row",
        1,
    ),
    named(
        "sessions.expand",
        Scope::Sessions,
        "←→",
        "expand",
        "Expand or collapse",
        12,
    ),
    bind(
        "sessions.terminal",
        Scope::Sessions,
        'o',
        "terminal",
        "Raise the session's terminal",
        2,
    ),
    bind(
        "sessions.new",
        Scope::Sessions,
        'n',
        "new",
        "New session in the selected folder",
        3,
    ),
    bind(
        "sessions.kill",
        Scope::Sessions,
        'x',
        "kill",
        "Kill the session, or the whole run",
        4,
    ),
    bind(
        "sessions.rename",
        Scope::Sessions,
        'r',
        "rename",
        "Rename the session (on a session row)",
        5,
    ),
    bind(
        "sessions.purge",
        Scope::Sessions,
        'X',
        "close done",
        "Close every session whose task has finished",
        8,
    ),
    bind(
        "sessions.add_group",
        Scope::Sessions,
        'a',
        "group",
        "Add a group",
        9,
    ),
    bind(
        "sessions.remove_group",
        Scope::Sessions,
        'd',
        "ungroup",
        "Remove the selected group",
        10,
    ),
    bind(
        "sessions.folders",
        Scope::Sessions,
        'F',
        "folders",
        "Show or hide folders with no live session",
        11,
    ),
    bind("sessions.settings", Scope::Sessions, 's', "", "Settings", 0),
    // --- right-hand pane ----------------------------------------------------
    named("conv.move", Scope::Conversation, "↑↓ j k", "", "Scroll", 0),
    named(
        "conv.page",
        Scope::Conversation,
        "Space PgDn PgUp",
        "",
        "Scroll a page",
        0,
    ),
    bind(
        "conv.search",
        Scope::Conversation,
        '/',
        "search",
        "Search the conversation",
        1,
    ),
    bind(
        "conv.timestamps",
        Scope::Conversation,
        't',
        "time",
        "Show or hide timestamps",
        2,
    ),
    bind(
        "conv.filter",
        Scope::Conversation,
        'f',
        "filter",
        "Show all messages, yours, or the agent's",
        3,
    ),
    bind(
        "conv.top",
        Scope::Conversation,
        'g',
        "top",
        "Jump to the top",
        4,
    ),
    bind(
        "conv.end",
        Scope::Conversation,
        'G',
        "end",
        "Jump to the newest line and follow it",
        5,
    ),
    bind("conv.settings", Scope::Conversation, 's', "", "Settings", 0),
    // --- board ----------------------------------------------------------------
    named("board.move", Scope::Board, "↑↓ j k", "", "Move", 0),
    named(
        "board.menu",
        Scope::Board,
        "Enter",
        "menu",
        "The row's menu",
        1,
    ),
    named(
        "board.expand",
        Scope::Board,
        "←→",
        "expand",
        "Expand or collapse",
        25,
    ),
    bind(
        "board.start",
        Scope::Board,
        's',
        "start",
        "Start development work on the task",
        2,
    )
    .only(NOT_QA)
    .note(DEV_WORK),
    bind(
        "board.qa_run",
        Scope::Board,
        'R',
        "QA run",
        "Watch the stage as a QA run, or drop the run",
        2,
    )
    .only(NOT_DEV),
    bind(
        "board.next_ask",
        Scope::Board,
        ']',
        "next ask",
        "Jump to the next agent waiting on you",
        3,
    )
    .only(NOT_DEV),
    bind(
        "board.answer",
        Scope::Board,
        'a',
        "answer",
        "Answer the selected question or verdict as you",
        4,
    ),
    bind(
        "board.revise",
        Scope::Board,
        'v',
        "revise",
        "Resume the task's session for a revision",
        5,
    )
    .only(NOT_QA)
    .note(DEV_WORK),
    bind(
        "board.chat",
        Scope::Board,
        'C',
        "chat",
        "Reopen the task's conversation with no prompt",
        6,
    )
    .only(NOT_QA)
    .note(DEV_WORK),
    bind(
        "board.stage",
        Scope::Board,
        'm',
        "stage",
        "Move the task to another stage",
        7,
    ),
    bind(
        "board.session",
        Scope::Board,
        'g',
        "session",
        "Go to the task's session",
        8,
    ),
    bind(
        "board.terminal",
        Scope::Board,
        'G',
        "terminal",
        "Raise the task session's terminal",
        9,
    ),
    bind(
        "board.browser",
        Scope::Board,
        'o',
        "browser",
        "Open the task in Odoo",
        10,
    ),
    bind(
        "board.filter",
        Scope::Board,
        'f',
        "filter",
        "Show your tasks, or everyone's",
        11,
    ),
    bind(
        "board.projects",
        Scope::Board,
        'p',
        "projects",
        "Pick the projects shown",
        12,
    ),
    bind(
        "board.auto_qa",
        Scope::Board,
        'A',
        "auto QA",
        "Switch Auto QA for the row's project, on this machine",
        13,
    )
    .only(NOT_DEV),
    bind(
        "board.dismiss",
        Scope::Board,
        'x',
        "dismiss",
        "Dismiss the selected notification, or all of them",
        14,
    ),
    bind(
        "board.pipeline",
        Scope::Board,
        'P',
        "pipeline",
        "The project's pipeline steps",
        15,
    )
    .only(NOT_QA),
    bind(
        "board.ssh",
        Scope::Board,
        'S',
        "ssh",
        "SSH to the project's server",
        16,
    )
    .only(NOT_QA),
    bind(
        "board.mrs",
        Scope::Board,
        'M',
        "MRs",
        "Your open merge requests",
        17,
    )
    .only(NOT_QA),
    bind(
        "board.daemon_logs",
        Scope::Board,
        'D',
        "daemon logs",
        "The auto-dev daemon's run logs for the task",
        18,
    )
    .only(NOT_QA),
    // --- deploy ---------------------------------------------------------------
    named("deploy.move", Scope::Deploy, "↑↓ j k", "", "Move", 0),
    named(
        "deploy.menu",
        Scope::Deploy,
        "Enter",
        "menu",
        "The row's menu",
        1,
    ),
    named(
        "deploy.expand",
        Scope::Deploy,
        "←→",
        "expand",
        "Expand or collapse",
        20,
    ),
    bind(
        "deploy.merge",
        Scope::Deploy,
        'm',
        "merge",
        "Merge the task's MR",
        2,
    ),
    bind(
        "deploy.merge_all",
        Scope::Deploy,
        'M',
        "merge all",
        "Merge every ready MR in the project",
        3,
    ),
    bind(
        "deploy.deploy",
        Scope::Deploy,
        'd',
        "deploy",
        "Deploy the project",
        4,
    ),
    bind(
        "deploy.cancel",
        Scope::Deploy,
        'X',
        "cancel",
        "Cancel the running deploy",
        5,
    ),
    bind(
        "deploy.output",
        Scope::Deploy,
        'L',
        "output",
        "The running deploy's output",
        6,
    ),
    bind(
        "deploy.conflicts",
        Scope::Deploy,
        'R',
        "conflicts",
        "Resolve the MR's merge conflicts",
        7,
    )
    .only(NOT_QA),
    bind(
        "deploy.session",
        Scope::Deploy,
        'g',
        "session",
        "Go to the task's session",
        8,
    ),
    bind(
        "deploy.terminal",
        Scope::Deploy,
        'G',
        "terminal",
        "Raise the task session's terminal",
        9,
    ),
    bind(
        "deploy.mr",
        Scope::Deploy,
        'o',
        "MR",
        "Open the MR in a browser",
        10,
    ),
    bind(
        "deploy.task",
        Scope::Deploy,
        't',
        "task",
        "Open the task in Odoo",
        11,
    ),
    bind(
        "deploy.config",
        Scope::Deploy,
        'c',
        "config",
        "The project's deploy settings",
        12,
    ),
];

/// Characters no binding may take: moving, scrolling, and the keys that
/// close or explain things.
const RESERVED: [char; 6] = ['j', 'k', ' ', 'q', 'Q', '?'];

/// What [`Keymap::route`] did with a key for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// Hand this to the scope's handler: the key as the handler knows it.
    Key(KeyEvent),
    /// Not this scope's key. Try the next scope.
    Skip,
    /// This scope's key, but not for the role. The reason was said.
    Refused(String),
}

/// The bindings with the person's remaps applied, for one role. The default
/// is the developer role with no remaps.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    role: UserRole,
    /// Binding id -> the key the person chose. Only valid remaps are kept.
    remaps: BTreeMap<&'static str, char>,
}

impl Keymap {
    /// `overrides` is the config's `keys` block: binding id -> one character.
    /// An unknown id, a fixed binding, a reserved character or a value that is
    /// not one character is ignored, so a bad line never takes a key away.
    pub fn new(role: UserRole, overrides: &BTreeMap<String, String>) -> Self {
        let mut remaps = BTreeMap::new();
        for (id, value) in overrides {
            let mut chars = value.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                continue;
            };
            if RESERVED.contains(&ch) || ch.is_control() {
                continue;
            }
            if let Some(binding) = BINDINGS.iter().find(|b| b.id == id && !b.fixed) {
                remaps.insert(binding.id, ch);
            }
        }
        Keymap { role, remaps }
    }

    pub fn role(&self) -> UserRole {
        self.role
    }

    /// The key a binding answers to now.
    pub fn key_of(&self, binding: &Binding) -> Key {
        match self.remaps.get(binding.id) {
            Some(ch) => Key::Char(*ch),
            None => binding.key,
        }
    }

    /// The key a binding answers to now, as text for the screen.
    pub fn label(&self, binding: &Binding) -> String {
        match self.key_of(binding) {
            Key::Char(ch) => ch.to_string(),
            Key::Named(name) => name.to_string(),
        }
    }

    /// The key a binding answers to now, by id, for a sentence that names
    /// it: "Press {} to retry".
    pub fn key(&self, id: &str) -> String {
        debug_assert!(Self::binding(id).is_some(), "no binding {id}");
        Self::binding(id)
            .map(|binding| self.label(binding))
            .unwrap_or_default()
    }

    /// The key as a character, for text built where a `Copy` value is
    /// needed. A named key has none, and reads as `?`.
    pub fn char_of(&self, id: &str) -> char {
        match Self::binding(id).map(|binding| self.key_of(binding)) {
            Some(Key::Char(ch)) => ch,
            _ => '?',
        }
    }

    /// `key word` for each of these bindings the role is offered, joined by
    /// `sep`: "m merge  ·  g session".
    pub fn line_of(&self, ids: &[&str], sep: &str) -> String {
        ids.iter()
            .filter_map(|id| Self::binding(id))
            .filter(|binding| binding.roles.has(self.role))
            .map(|binding| format!("{} {}", self.label(binding), binding.hint))
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// The binding with this id.
    pub fn binding(id: &str) -> Option<&'static Binding> {
        BINDINGS.iter().find(|binding| binding.id == id)
    }

    /// Turn a pressed key into what one scope's handler expects.
    ///
    /// A remapped binding's key comes back as its default, which is what the
    /// handler matches on. A default key that was remapped away is skipped, so
    /// it no longer does the old thing. Any other key passes unchanged.
    pub fn route(&self, scope: Scope, key: KeyEvent) -> Routed {
        let KeyCode::Char(pressed) = key.code else {
            return Routed::Key(key);
        };
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Routed::Key(key);
        }
        let in_scope = || BINDINGS.iter().filter(move |b| b.scope == scope);
        let hit = in_scope().find(|b| self.key_of(b) == Key::Char(pressed));
        let Some(binding) = hit else {
            let moved_away = in_scope().any(|b| b.key == Key::Char(pressed));
            return if moved_away {
                Routed::Skip
            } else {
                Routed::Key(key)
            };
        };
        if !binding.roles.has(self.role) {
            let mut reason = format!(
                "The {} role does not offer `{pressed}` ({}).",
                self.role.as_str().to_uppercase(),
                binding.help.to_lowercase()
            );
            if !binding.note.is_empty() {
                reason.push(' ');
                reason.push_str(binding.note);
            }
            return Routed::Refused(reason);
        }
        let Key::Char(default) = binding.key else {
            return Routed::Key(key);
        };
        Routed::Key(KeyEvent {
            code: KeyCode::Char(default),
            ..key
        })
    }

    /// The bindings this role is offered in a scope, in table order.
    pub fn offered(&self, scope: Scope) -> impl Iterator<Item = &'static Binding> + '_ {
        BINDINGS
            .iter()
            .filter(move |b| b.scope == scope && b.roles.has(self.role))
    }

    /// Why `ch` cannot be given to `id`. `None` when it can.
    ///
    /// Two keys clash when they are equal and their scopes overlap. A clash
    /// the defaults already have is allowed, so putting a key back is always
    /// possible: `r` is both refresh and rename by design, and rename only
    /// takes it on a session row.
    pub fn refuse(&self, id: &str, ch: char) -> Option<String> {
        let binding = Self::binding(id)?;
        if binding.fixed {
            return Some(format!("{} cannot be changed.", binding.help));
        }
        if RESERVED.contains(&ch) || ch.is_control() || ch.is_whitespace() {
            return Some(format!(
                "`{ch}` is kept for moving, closing or help. Pick another key."
            ));
        }
        let clash = BINDINGS.iter().find(|other| {
            other.id != binding.id
                && other.scope.overlaps(binding.scope)
                && self.key_of(other) == Key::Char(ch)
                && !(binding.key == Key::Char(ch) && other.key == Key::Char(ch))
        })?;
        Some(format!(
            "`{ch}` is already {} ({}). Change that one first.",
            clash.help.to_lowercase(),
            clash.scope.title().to_lowercase()
        ))
    }
}

/// The scopes whose keys work now, the one that answers first first.
pub fn active_scopes(view: View, focus: Pane) -> Vec<Scope> {
    let view_scope = match view {
        View::Sessions => Scope::Sessions,
        View::Board => Scope::Board,
        View::Deploy => Scope::Deploy,
    };
    match (view, focus) {
        // The sessions list ignores its keys while the pane has the focus.
        (View::Sessions, Pane::Conversation) => vec![Scope::Conversation],
        (_, Pane::Conversation) => vec![Scope::Conversation, view_scope],
        (_, Pane::Tree) => vec![view_scope],
    }
}

/// The status bar's hints for what works now, most useful first. `?` and `q`
/// are not in it: the bar always ends with them.
pub fn hints(keymap: &Keymap, view: View, focus: Pane) -> Vec<(String, &'static str)> {
    let mut scopes = active_scopes(view, focus);
    scopes.push(Scope::Global);
    let mut offered: Vec<(usize, &Binding)> = Vec::new();
    for (depth, scope) in scopes.iter().enumerate() {
        for binding in keymap.offered(*scope) {
            if binding.rank == 0 || binding.hint.is_empty() {
                continue;
            }
            // A key an earlier scope already answers is not this one's.
            let taken = offered
                .iter()
                .any(|(_, b)| keymap.key_of(b) == keymap.key_of(binding));
            if !taken {
                offered.push((depth, binding));
            }
        }
    }
    // By rank, and on a tie the scope that answers first.
    offered.sort_by_key(|(depth, b)| (b.rank, *depth));
    offered
        .into_iter()
        .map(|(_, b)| (keymap.label(b), b.hint))
        .collect()
}

/// The hints that fit in `width` columns, with `? keys` and `q quit` always
/// last. Each hint takes two spaces, its key, a space and its word.
pub fn fit_hints(
    keymap: &Keymap,
    hints: Vec<(String, &'static str)>,
    width: usize,
) -> Vec<(String, &'static str)> {
    let cost = |(key, word): &(String, &str)| 3 + key.chars().count() + word.chars().count();
    let tail: Vec<(String, &'static str)> = ["global.help", "global.quit"]
        .iter()
        .filter_map(|id| Keymap::binding(id))
        .map(|b| (keymap.label(b), b.hint))
        .collect();
    let mut left = width.saturating_sub(tail.iter().map(cost).sum());
    let mut out = Vec::new();
    for hint in hints {
        let need = cost(&hint);
        if need > left {
            continue;
        }
        left -= need;
        out.push(hint);
    }
    out.extend(tail);
    out
}

/// A scope's character keys for the role, as `key word` pairs, a few to a
/// line. For the placeholder text an empty right-hand pane shows.
pub fn key_lines(keymap: &Keymap, scope: Scope, per_line: usize) -> Vec<String> {
    let pairs: Vec<String> = keymap
        .offered(scope)
        .filter(|b| !b.hint.is_empty() && matches!(b.key, Key::Char(_)))
        .map(|b| format!("{} {}", keymap.label(b), b.hint))
        .collect();
    pairs
        .chunks(per_line.max(1))
        .map(|chunk| chunk.join("   "))
        .collect()
}

/// The `?` help's sections for a view: the scopes that answer on it, then
/// everywhere. Each line is the key as it is now, and what it does.
pub fn help_sections(keymap: &Keymap, view: View) -> Vec<(&'static str, Vec<(String, String)>)> {
    let view_scope = match view {
        View::Sessions => Scope::Sessions,
        View::Board => Scope::Board,
        View::Deploy => Scope::Deploy,
    };
    [view_scope, Scope::Conversation, Scope::Global]
        .into_iter()
        .map(|scope| {
            let lines = keymap
                .offered(scope)
                .map(|b| (keymap.label(b), b.help.to_string()))
                .collect();
            (scope.title(), lines)
        })
        .collect()
}
