//! Terminal driver selection.
//!
//! Configure explicitly in `~/.claude-sessions.json`:
//!
//! ```json
//! "terminal": { "driver": "tmux", "tmuxSession": "claude-sessions" }
//! ```
//!
//! `auto` (the default) prefers iTerm2 on macOS — that is the historical
//! behaviour and what an existing install expects — and falls back to tmux when
//! iTerm2 cannot be driven, which is what makes non-macOS and SSH use possible.

use std::sync::{Arc, Mutex};

use crate::config::ConfigHandle;
use crate::types::TerminalDriverName;

use super::iterm2::Iterm2Driver;
use super::spawn::SpawnPolicy;
use super::tmux::TmuxDriver;
use super::types::{NullDriver, TerminalDriver};

/// Only the distinction the drivers care about. Passed in rather than read from
/// `cfg!`, so the selection policy is testable for a machine that is not this
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

/// A driver that actually exists. `auto` is a request, not an answer, so it has
/// no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    Iterm2,
    Tmux,
}

/// What could be driven on this machine right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverAvailability {
    pub iterm: bool,
    pub tmux: bool,
    pub platform: Platform,
}

/// Decide which driver to use given what is available.
///
/// Pure, so the policy is testable without either terminal installed — and so
/// that the one rule worth guarding is stated in one place: an explicit choice
/// is never silently replaced. Quietly driving a different terminal than the
/// one someone configured is worse than telling them the one they asked for is
/// missing, so an unavailable explicit choice resolves to nothing.
pub fn choose_driver(
    requested: TerminalDriverName,
    available: DriverAvailability,
) -> Option<DriverKind> {
    match requested {
        TerminalDriverName::Iterm2 => available.iterm.then_some(DriverKind::Iterm2),
        TerminalDriverName::Tmux => available.tmux.then_some(DriverKind::Tmux),
        TerminalDriverName::Auto => {
            if available.platform == Platform::MacOs && available.iterm {
                Some(DriverKind::Iterm2)
            } else if available.tmux {
                Some(DriverKind::Tmux)
            } else if available.iterm {
                Some(DriverKind::Iterm2)
            } else {
                None
            }
        }
    }
}

/// Build the named driver. Unlike the Node original there is no "unknown name"
/// case to return null for: an unrecognised config string has already been
/// resolved to [`TerminalDriverName::Auto`] by the config accessor.
pub fn make_driver(
    kind: DriverKind,
    tmux_session: &str,
    policy: SpawnPolicy,
) -> Arc<dyn TerminalDriver> {
    match kind {
        DriverKind::Tmux => Arc::new(TmuxDriver::new(tmux_session, policy)),
        DriverKind::Iterm2 => Arc::new(Iterm2Driver::new(policy)),
    }
}

type DriverCache = Mutex<Option<Option<Arc<dyn TerminalDriver>>>>;

/// Resolved once and kept: probing availability costs an `osascript` round trip,
/// and the answer does not change while the dashboard is running.
/// [`reset_driver_cache`] exists for the config watcher.
static CACHE: DriverCache = Mutex::new(None);

/// Resolve (and memoise) the active driver.
pub fn get_driver(config: &ConfigHandle, policy: SpawnPolicy) -> Option<Arc<dyn TerminalDriver>> {
    let mut cache = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(cached) = cache.as_ref() {
        return cached.clone();
    }

    let requested = config.terminal_driver();
    let tmux_session = config.tmux_session();

    // Only probe what the request could possibly select — an explicit `tmux`
    // must never wake iTerm2 up just to ask whether it is there.
    let available = DriverAvailability {
        iterm: requested != TerminalDriverName::Tmux && Iterm2Driver::new(policy).is_available(),
        tmux: requested != TerminalDriverName::Iterm2
            && TmuxDriver::new(tmux_session, policy).is_available(),
        platform: Platform::current(),
    };

    let chosen =
        choose_driver(requested, available).map(|kind| make_driver(kind, tmux_session, policy));
    *cache = Some(chosen.clone());
    chosen
}

pub fn reset_driver_cache() {
    *CACHE.lock().unwrap_or_else(|err| err.into_inner()) = None;
}

/// The driver, or one that reports a clear reason for every operation instead of
/// failing silently.
pub fn driver_or_null(config: &ConfigHandle, policy: SpawnPolicy) -> Arc<dyn TerminalDriver> {
    get_driver(config, policy).unwrap_or_else(|| Arc::new(NullDriver))
}
