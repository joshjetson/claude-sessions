//! What this build can actually do on the machine it is running on.
//!
//! Native Windows is supported the way an early port is supported: everything
//! that is a file, a socket or a screen works — the dashboard, transcripts, the
//! daemon, the board, the deploy list, the journal — and everything that needs
//! a Unix process table or a Unix terminal does not, yet. Stating that is the
//! whole job of this module. A capability that is missing has to *say* it is
//! missing, in one sentence, where the person was already looking. What it must
//! never do is panic, and what it must never do is quietly nothing.
//!
//! The sentences live here rather than at the call sites so there is one
//! wording to change when a phase lands (WORKING.md rule 3), and the four flags
//! below double as the roadmap: each one names the phase that turns it on.
//!
//! Everything here is a compile-time constant, which means the Unix build pays
//! nothing for it and the branches fold away.

/// Reading the machine's process table: which `claude` processes are alive,
/// their working directory and their launch environment.
///
/// Unix does this with `ps` and `lsof`. Windows needs WMI command lines and a
/// PEB read for the cwd — the next Windows phase. Until then the sessions view
/// lists nothing live, which is why [`DISCOVERY_NOTICE`] exists.
pub const LIVE_DISCOVERY: bool = cfg!(unix);

/// Opening, typing into, focusing and closing a terminal: the tmux and iTerm2
/// drivers, and the `ps` tty that joins a discovered session to a drivable
/// pane.
///
/// A Windows Terminal driver is a later phase. Until then the null driver
/// answers every call with a reason and [`NO_TERMINAL_HINT`].
pub const TERMINAL_CONTROL: bool = cfg!(unix);

/// Signalling another process — what `x` on a session and a deploy cancel do.
///
/// Routed through `/bin/kill` on purpose, so the spawn policy gates it. Windows
/// has `taskkill`, but with no discovery there are no pids to send it to yet,
/// and an untested kill path is worse than an honest refusal.
pub const PROCESS_SIGNALS: bool = cfg!(unix);

/// Running a command under a LOGIN shell (`/bin/sh -lc`), which is how a deploy
/// finds `nvm`, `asdf`, `gcloud` and `kubectl`.
///
/// `cmd /C` is not a login shell and has no equivalent, so a Windows deploy
/// would run with a different PATH than the one the command was written
/// against. Refused rather than silently different.
pub const LOGIN_SHELL: bool = cfg!(unix);

/// Shown where a list of live sessions would be, when discovery cannot see any
/// processes at all. Deliberately says what still works: the transcripts are on
/// disk and being appended to, and every view that reads them is unaffected.
pub const DISCOVERY_NOTICE: &str = "Live session discovery is not yet supported on native \
                                    Windows — transcripts still appear as they update.";

/// What to tell someone who asked for a terminal and got no driver. On Unix
/// that is a machine without tmux or iTerm2; on Windows it is the phase.
pub const NO_TERMINAL_HINT: &str = if TERMINAL_CONTROL {
    "Install tmux, or run under iTerm2 on macOS."
} else {
    "Terminal control lands in a later Windows phase; tmux under WSL works today."
};

/// The notice for the sessions view, or `None` where discovery works.
pub fn discovery_notice() -> Option<&'static str> {
    (!LIVE_DISCOVERY).then_some(DISCOVERY_NOTICE)
}

/// The hint a driver result carries where terminal control is not implemented
/// yet, or `None` where it is — on Unix a missing terminal is a missing
/// terminal, not a missing phase, and the error already says so.
pub fn terminal_notice() -> Option<String> {
    (!TERMINAL_CONTROL).then(|| NO_TERMINAL_HINT.to_string())
}

/// One sentence for an action this platform cannot perform yet.
///
/// `action` is phrased as the thing being refused and capitalised, because it
/// starts a sentence a user reads: `unsupported("Signalling a session")`.
pub fn unsupported(action: &str) -> String {
    format!("{action} is not supported yet on native Windows.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_the_action_and_the_platform() {
        assert_eq!(
            unsupported("Signalling a session"),
            "Signalling a session is not supported yet on native Windows."
        );
    }

    #[test]
    fn the_notices_appear_exactly_where_the_capability_is_missing() {
        assert_eq!(discovery_notice().is_some(), !LIVE_DISCOVERY);
        assert_eq!(terminal_notice().is_some(), !TERMINAL_CONTROL);
    }

    /// The flags are a statement about this build, and the tests below are what
    /// keeps them from drifting into a lie after an edit.
    #[cfg(unix)]
    #[test]
    fn unix_is_fully_supported() {
        const { assert!(LIVE_DISCOVERY && TERMINAL_CONTROL && PROCESS_SIGNALS && LOGIN_SHELL) };
        assert_eq!(discovery_notice(), None);
        assert_eq!(terminal_notice(), None);
        assert!(NO_TERMINAL_HINT.contains("tmux"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_degrades_and_says_so() {
        const { assert!(!LIVE_DISCOVERY && !TERMINAL_CONTROL && !PROCESS_SIGNALS && !LOGIN_SHELL) };
        assert!(discovery_notice().unwrap().contains("native Windows"));
        assert!(terminal_notice().unwrap().contains("WSL"));
    }
}
