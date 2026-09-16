//! The one gate every child process in this crate passes through.

use std::env;
use std::fmt;

/// Environment variable that disables every spawn for anything that is not a
/// `cargo test` process: CI, a scripted smoke run, a demo recording.
pub const NO_SPAWN_ENV: &str = "CLAUDE_SESSIONS_NO_SPAWN";

/// Whether this process may start child processes and drive terminals.
///
/// Injected rather than read at each call site, so a test can hand a refusing
/// policy to code that has no idea it is under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPolicy {
    Allow,
    Refuse,
}

impl SpawnPolicy {
    /// What `main` uses: refuse when the environment says so, and refuse
    /// unconditionally inside a unit-test binary.
    ///
    /// `cfg!(test)` covers the unit tests in this crate. Integration tests
    /// compile the library without it, so they construct [`SpawnPolicy::Refuse`]
    /// explicitly — or [`SpawnPolicy::Allow`] when, as in the live tmux suite,
    /// driving a real terminal is the thing under test.
    pub fn detect() -> Self {
        if cfg!(test) || env::var(NO_SPAWN_ENV).as_deref() == Ok("1") {
            SpawnPolicy::Refuse
        } else {
            SpawnPolicy::Allow
        }
    }

    pub fn is_allowed(self) -> bool {
        self == SpawnPolicy::Allow
    }

    /// THE choke point. Every `Command` this crate starts is preceded by a
    /// `check`, so there is exactly one place that can be taught a new rule and
    /// exactly one place a future author would have to defeat to get it wrong.
    ///
    /// Why it exists: in the Node app the guard lived at the launch helper, and
    /// a test that walked past the start gate reached the real launch path,
    /// found the user's live project-to-directory mapping, and opened terminal
    /// tabs running `claude --dangerously-skip-permissions` against production
    /// task data. Sixteen prompt files and several running agents came out of
    /// test runs before anyone noticed. A guard the tests own can be forgotten
    /// by the next test; a guard the spawn itself owns cannot.
    ///
    /// `action` is phrased as the thing being refused ("open an ssh session"),
    /// because this message is shown to a user, not only to a developer.
    pub fn check(self, action: &str) -> Result<(), SpawnRefused> {
        match self {
            SpawnPolicy::Allow => Ok(()),
            SpawnPolicy::Refuse => Err(SpawnRefused {
                message: format!(
                    "Refusing to {action}: spawning is disabled in this process \
                     (test run, or {NO_SPAWN_ENV}=1)."
                ),
            }),
        }
    }
}

/// A refusal, carrying the sentence the caller shows the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRefused {
    pub message: String,
}

impl fmt::Display for SpawnRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SpawnRefused {}
