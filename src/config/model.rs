//! The on-disk shape of `~/.claude-sessions.json`.
//!
//! Two rules run through this file, both in service of never damaging a config
//! the tool did not write:
//!
//! 1. **Unknown keys survive.** Every block carries a flattened catch-all, so a
//!    setting from a newer version — or a comment-ish key someone parked there —
//!    comes back out of a load/save round-trip unchanged.
//! 2. **Settings with a fixed vocabulary are stored as strings**, not enums. A
//!    typo'd `"defaultView": "kanban"` round-trips and falls back at read time;
//!    a typed enum would fail the parse and lose the whole file.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Unknown keys, kept verbatim so they survive a save.
pub type JsonMap = serde_json::Map<String, Value>;

pub const DEFAULT_CONVERSATION_WIDTH: u16 = 25;
/// The conversation pane is a percentage of the screen; outside this range one
/// of the two panes stops being usable.
pub const MIN_CONVERSATION_WIDTH: u16 = 10;
pub const MAX_CONVERSATION_WIDTH: u16 = 80;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Folders the sessions tree groups by. Always written, even when empty —
    /// that is what the Node app's loader guaranteed downstream.
    #[serde(deserialize_with = "lenient")]
    pub groups: Vec<Group>,
    #[serde(deserialize_with = "lenient")]
    pub chat: ChatConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub odoo: Option<OdooBlock>,
    /// Odoo project name -> one or more local repo paths.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "lenient")]
    pub odoo_project_dirs: BTreeMap<String, OneOrMany>,
    /// Odoo project name -> the branch its merge requests target.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "lenient")]
    pub target_branches: BTreeMap<String, String>,
    /// Host `glab` talks to. No default: this is a public build, and the only
    /// correct value is the one the person using it configures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitlab_host: Option<String>,
    /// Odoo project name -> ssh alias, for projects whose name does not resemble
    /// their host.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "lenient")]
    pub ssh_hosts: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<UsageBlock>,
    /// Sessions-tab settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub sessions: Option<SessionsBlock>,
    /// QA run settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub qa: Option<QaBlock>,
    /// Memory sampling into `runtime/memory.log`. Off unless asked for — it is
    /// a diagnostic, not a feature. See [`crate::diagnostics`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<bool>,
    /// `sessions` | `board` | `deploy`; validated in the accessor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_view: Option<String>,
    /// Legacy sibling of `daemon.port`, still honoured.
    #[serde(
        default,
        deserialize_with = "de_port",
        skip_serializing_if = "Option::is_none"
    )]
    pub notify_port: Option<u16>,
    /// Stage a finished task moves to. A list is tried in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_stage: Option<OneOrMany>,
    /// Stage a task moves to when work starts on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress_stage: Option<OneOrMany>,
    /// Session id -> the name you gave that session.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "lenient")]
    pub nicknames: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub board: Option<BoardBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub sounds: Option<SoundsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub deploy: Option<DeployBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub optics: Option<OpticsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub alerts: Option<AlertsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub terminal: Option<TerminalBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    pub daemon: Option<DaemonBlock>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Group {
    pub name: String,
    pub path: String,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Group {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Group {
            name: name.into(),
            path: path.into(),
            extra: JsonMap::new(),
        }
    }
}

/// Conversation-pane settings. Field order matches the Node defaults so a saved
/// file reads the same as one the old tool wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatConfig {
    pub theme: String,
    pub user_color: String,
    pub assistant_color: String,
    pub tool_color: String,
    pub code_color: String,
    pub user_label: String,
    pub assistant_label: String,
    pub show_timestamps: bool,
    /// `show` | `hide` | `collapse`.
    pub tool_display: String,
    pub compact_mode: bool,
    /// 0 means unlimited.
    #[serde(deserialize_with = "de_i64")]
    pub max_lines_per_message: i64,
    /// `all` | `user` | `assistant`.
    pub message_filter: String,
    pub search_keyword: String,
    /// Percentage of the width given to the conversation pane, clamped on load.
    #[serde(deserialize_with = "de_conversation_width")]
    pub conversation_width: u16,
    pub swap_panels: bool,
    pub show_session_header: bool,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl Default for ChatConfig {
    fn default() -> Self {
        ChatConfig {
            theme: "default".into(),
            user_color: "cyan".into(),
            assistant_color: "green".into(),
            tool_color: "yellow".into(),
            code_color: "magenta".into(),
            user_label: "You".into(),
            assistant_label: "Claude".into(),
            show_timestamps: false,
            tool_display: "show".into(),
            compact_mode: false,
            max_lines_per_message: 0,
            message_filter: "all".into(),
            search_keyword: String::new(),
            conversation_width: DEFAULT_CONVERSATION_WIDTH,
            swap_panels: false,
            show_session_header: true,
            extra: JsonMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OdooBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 0 (the default) means "only when asked".
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "de_number")]
    pub interval_minutes: Option<serde_json::Number>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

/// Per-level notification sounds, as paths `afplay` can open.
///
/// New in the port. The Node app bundled an mp3 in the repository for the
/// success chime and hardcoded three macOS system sounds for the rest; a
/// bundled asset is not something a published crate should carry, so every
/// level is a path and the defaults are all system sounds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SoundsBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    /// `false` silences every level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BoardBlock {
    /// When non-empty, the "all" view loads only these projects.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Projects always hidden, in both "mine" and "all".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
    /// `None` and `Some([])` differ here: absent means the default
    /// (`["Deployed"]`), an empty list means hide nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_stages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_states: Option<Vec<String>>,
    /// Legacy toggle, still honoured when `hideStates` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_done_state: Option<bool>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeployBlock {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, DeployProject>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

/// A deploy entry is either the command on its own or the full settings object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeployProject {
    /// `"Widgets": "./scripts/deploy-prod.sh"` — shorthand for `{ command }`.
    Command(String),
    Settings(DeployProjectConfig),
}

impl DeployProject {
    pub fn settings(&self) -> DeployProjectConfig {
        match self {
            DeployProject::Command(command) => DeployProjectConfig {
                command: Some(command.clone()),
                ..Default::default()
            },
            DeployProject::Settings(cfg) => cfg.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeployProjectConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Defaults to the project's first mapped repo dir. `~` is expanded on read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpticsBlock {
    /// No default endpoint and no default token: both are deployment-specific.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Odoo project name -> Optics sdk_key.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlertsBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Stages that mean "this is yours to pick up now". Empty falls back to the
    /// default, exactly as an absent key does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_task_stages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "de_number")]
    pub stuck_after_minutes: Option<serde_json::Number>,
    /// 0 means "flag it once and stop".
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "de_number")]
    pub remind_every_minutes: Option<serde_json::Number>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerminalBlock {
    /// `auto` | `iterm2` | `tmux`; validated in the accessor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DaemonBlock {
    #[serde(deserialize_with = "de_port", skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Start a daemon when none is reachable. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

/// A setting that accepts either one value or a list of them. The shape is kept
/// as written so a hand-edited single string does not become a list on save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn as_slice(&self) -> &[String] {
        match self {
            OneOrMany::One(value) => std::slice::from_ref(value),
            OneOrMany::Many(values) => values,
        }
    }
}

impl From<Vec<String>> for OneOrMany {
    fn from(values: Vec<String>) -> Self {
        OneOrMany::Many(values)
    }
}

impl From<String> for OneOrMany {
    fn from(value: String) -> Self {
        OneOrMany::One(value)
    }
}

// --- forgiving deserialisers ------------------------------------------------

/// A value of the wrong *shape* falls back to the default rather than failing
/// the whole file — the Node loader did the same for `groups`, and losing every
/// other setting to one bad key is not a trade worth making.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// Node coerced config numbers with `Number(x)`, so `"8787"` worked as well as
/// `8787`. Preserved: a quoted number is a typo, not a reason to reset a config.
fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// A minute count. `"30"` is a typo, not a reason to drop the block it is in —
/// Node read every number through `Number(x)`, which took both spellings.
fn de_number<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Number>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(match &value {
        Value::Number(number) => Some(number.clone()),
        _ => as_f64(&value).and_then(serde_json::Number::from_f64),
    })
}

fn de_port<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u16>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(as_f64(&value)
        .filter(|n| *n >= 1.0 && *n <= u16::MAX as f64)
        .map(|n| n.round() as u16))
}

fn de_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(as_f64(&value).map(|n| n.round() as i64).unwrap_or(0))
}

/// Clamped on load, because every consumer wants a usable percentage and none
/// of them should have to remember to clamp it. Zero (and anything unreadable)
/// means "unset" and takes the default, which is how the Node UI read it.
fn de_conversation_width<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let raw = as_f64(&value).unwrap_or(0.0);
    if raw <= 0.0 {
        return Ok(DEFAULT_CONVERSATION_WIDTH);
    }
    Ok(
        (raw.round() as i64).clamp(MIN_CONVERSATION_WIDTH as i64, MAX_CONVERSATION_WIDTH as i64)
            as u16,
    )
}

/// How a QA run behaves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QaBlock {
    /// How many QA sessions a run may have live at once.
    ///
    /// Caps CONCURRENCY, not the size of a run: a run of 15 tasks with a limit
    /// of 6 starts 6 and leaves the rest queued until a lane frees.
    ///
    /// Absent or `0` means no cap, and that is the default. Two earlier drafts
    /// of this feature capped it at 4 and then 16, both reasoned from a guess
    /// about how many browsers a machine tolerates rather than from a
    /// measurement — and both sat below what the tool is already used for. A
    /// cap below someone's normal working volume is an obstacle, not a
    /// safeguard.
    ///
    /// What stays enforced regardless: one pass per task. Two passes on one
    /// task share a worktree path and tear each other's checkout down, and no
    /// concurrency number would catch that.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_limit: Option<usize>,
}

/// How the sessions tree is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionsBlock {
    /// Show every folder in a group that has no live session, greyed out.
    ///
    /// Off by default. A group of eighteen checkouts draws eighteen grey rows
    /// and buries the two projects actually running — and the rows earn their
    /// place only when you want to START something in a quiet repo, which is
    /// the minority of the time you are looking at this tab.
    ///
    /// They are not gone: `F` toggles them, and the toggle is written back
    /// here, so turning them on once makes it stick.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_inactive_folders: Option<bool>,
}
