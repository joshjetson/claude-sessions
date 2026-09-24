//! `~/.claude-sessions.json`, loaded once and kept.
//!
//! The Node version re-read and re-parsed the file inside *every* accessor —
//! including `getSessionNicknames()`, which the sessions tree called once per
//! rendered row per frame. Here the file is read on [`ConfigHandle::load`] and
//! the accessors are field reads; [`ConfigHandle::reload`] exists for the file
//! watcher, and every mutator updates the cache and the file together.
//!
//! Environment handling is split by intent and captured once in
//! [`EnvOverrides`]: `ODOO_*` are fallbacks that config beats per field, while
//! `GITLAB_HOST`, `OPTICS_API`, `OPTICS_TOKEN` and `CLAUDE_SESSIONS_NOTIFY_PORT`
//! override it.

mod edit;
mod env;
mod model;
mod resolve;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use edit::DeployProjectPatch;
pub use env::EnvOverrides;
pub use model::*;
pub use resolve::{
    AlertConfig, BoardHideFilter, BoardProjectFilter, QaAlertConfig, ResolvedDeployConfig,
    UsageConfig,
};

use crate::paths::Paths;

/// The port everything falls back to when nothing else names one.
pub const DEFAULT_PORT: u16 = 8787;
/// tmux session the dashboard groups its windows under.
pub const DEFAULT_TMUX_SESSION: &str = "claude-sessions";
const DEFAULT_NEW_TASK_STAGE: &str = "Approved to Start";
/// Stages that mean "waiting on QA", from the QA Board this alert was ported
/// from. Projects spell the stage differently, and `Tech Debt Work` is the one
/// project lane where debt tasks wait for review. A project with its own QA
/// stage name adds it through `qa.newTaskStages`.
pub const DEFAULT_QA_STAGES: [&str; 3] = ["QA", "Quality Assurance", "Tech Debt Work"];
/// Stages that mean "QA sent it back and a developer is fixing it". Every
/// spelling here is live in at least one project. A task in one of these is a
/// developer's queue, so it never counts as a QA arrival, even when someone
/// lists the stage in `qa.newTaskStages`.
pub const REVISION_STAGES: [&str; 4] = [
    "Revision Required",
    "Revisions Required",
    "Revision Needed",
    "Required Revisions",
];
const DEFAULT_HIDE_STAGE: &str = "Deployed";
/// Odoo `state` values hidden from the board by default: "Done" (the checkmark)
/// and "Cancelled". "Complete" (`03_approved`) stays visible.
const DEFAULT_HIDE_STATES: [&str; 2] = ["1_done", "1_canceled"];

/// A loaded config plus the environment it resolves against.
#[derive(Debug, Clone)]
pub struct ConfigHandle {
    path: PathBuf,
    home: PathBuf,
    env: EnvOverrides,
    config: Config,
}

impl ConfigHandle {
    /// Reads the file. A missing or unreadable one yields defaults — the
    /// dashboard has to start on a machine that has never been configured.
    pub fn load(paths: &Paths, env: EnvOverrides) -> Self {
        ConfigHandle::load_from(&paths.config_path, &paths.home, env)
    }

    /// The same, against an explicit path and home directory, so tests get a
    /// real file without touching the user's.
    pub fn load_from(path: &Path, home: &Path, env: EnvOverrides) -> Self {
        let mut handle = ConfigHandle {
            path: path.to_path_buf(),
            home: home.to_path_buf(),
            env,
            config: Config::default(),
        };
        handle.reload();
        handle
    }

    /// Re-read from disk, for the file watcher and for `r` in the dashboard.
    pub fn reload(&mut self) {
        self.config = fs::read_to_string(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
    }

    /// A freshly read handle for the same file.
    ///
    /// For the long-lived closures the daemon is built from — the board and
    /// deploy polls — which own a snapshot the user can edit out from under
    /// them. They are built before the engine exists, so they cannot borrow its
    /// handle, and a board filter or a deploy command edited in a dialog has to
    /// change what the next poll asks for without a restart.
    pub fn reloaded(&self) -> ConfigHandle {
        ConfigHandle::load_from(&self.path, &self.home, self.env.clone())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn env(&self) -> &EnvOverrides {
        &self.env
    }

    /// Two-space JSON with a trailing newline — byte-comparable with what the
    /// Node app wrote, so switching between them does not churn the file.
    pub fn save(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.config).map_err(io::Error::other)?;
        fs::write(&self.path, json + "\n")
    }
}

// --- shared key handling ----------------------------------------------------

/// Config maps are keyed by Odoo project name, which people type by hand and
/// Odoo renders with its own capitalisation. Exact match wins; otherwise the
/// first case-insensitive one does. The Node app inlined this five times.
pub fn lookup_ci<'a, V>(map: &'a BTreeMap<String, V>, key: &str) -> Option<(&'a str, &'a V)> {
    if let Some((stored, value)) = map.get_key_value(key) {
        return Some((stored.as_str(), value));
    }
    let wanted = key.to_lowercase();
    map.iter()
        .find(|(stored, _)| stored.to_lowercase() == wanted)
        .map(|(stored, value)| (stored.as_str(), value))
}

/// `~` expansion, matching the Node behaviour exactly: only a leading tilde, and
/// the rest of the string is kept as written.
fn expand_tilde(path: &str, home: &Path) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", home.display()),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests;
