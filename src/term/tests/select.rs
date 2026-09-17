//! Which driver, given what exists on this machine.

use crate::term::{
    choose_driver, make_driver, DriverAvailability, DriverKind, NullDriver, Platform, SessionRef,
    SpawnPolicy, TerminalDriver,
};
use crate::types::TerminalDriverName;

fn available(iterm: bool, tmux: bool, platform: Platform) -> DriverAvailability {
    DriverAvailability {
        iterm,
        tmux,
        platform,
    }
}

#[test]
fn auto_prefers_iterm2_on_macos_so_existing_installs_do_not_change_behaviour() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Auto,
            available(true, true, Platform::MacOs)
        ),
        Some(DriverKind::Iterm2)
    );
}

#[test]
fn auto_falls_back_to_tmux_when_iterm2_cannot_be_driven() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Auto,
            available(false, true, Platform::MacOs)
        ),
        Some(DriverKind::Tmux)
    );
}

#[test]
fn auto_picks_tmux_off_macos() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Auto,
            available(false, true, Platform::Other)
        ),
        Some(DriverKind::Tmux)
    );
}

#[test]
fn auto_uses_iterm2_off_macos_only_when_nothing_else_is_there() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Auto,
            available(true, false, Platform::Other)
        ),
        Some(DriverKind::Iterm2)
    );
}

#[test]
fn an_explicit_choice_is_never_silently_overridden() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Tmux,
            available(true, true, Platform::MacOs)
        ),
        Some(DriverKind::Tmux)
    );
    assert_eq!(
        choose_driver(
            TerminalDriverName::Iterm2,
            available(true, true, Platform::Other)
        ),
        Some(DriverKind::Iterm2)
    );
}

#[test]
fn an_explicit_choice_that_is_unavailable_resolves_to_nothing_not_a_fallback() {
    // Silently using a different terminal than the one configured would be
    // worse than reporting that it is missing.
    assert_eq!(
        choose_driver(
            TerminalDriverName::Tmux,
            available(true, false, Platform::MacOs)
        ),
        None
    );
    assert_eq!(
        choose_driver(
            TerminalDriverName::Iterm2,
            available(false, true, Platform::MacOs)
        ),
        None
    );
}

#[test]
fn nothing_available_resolves_to_nothing() {
    assert_eq!(
        choose_driver(
            TerminalDriverName::Auto,
            available(false, false, Platform::Other)
        ),
        None
    );
}

#[test]
fn make_driver_returns_the_named_driver() {
    let policy = SpawnPolicy::Refuse;
    assert_eq!(
        make_driver(DriverKind::Tmux, "claude-sessions", policy).name(),
        "tmux"
    );
    assert_eq!(
        make_driver(DriverKind::Iterm2, "claude-sessions", policy).name(),
        "iterm2"
    );
    // The Node original also had to answer for makeDriver('nope'); here an
    // unrecognised config string never reaches this function, because the
    // config accessor has already resolved it to `auto`.
}

#[test]
fn the_null_driver_reports_a_reason_for_every_operation() {
    let null = NullDriver;
    let session = SessionRef::from_tty("ttys004");
    let request = crate::term::LaunchRequest::new("/repo", "claude");
    for result in [
        null.launch(&request),
        null.send_text(&session, "x"),
        null.focus(&session),
        null.close(&session),
    ] {
        assert!(!result.ok);
        assert!(
            result
                .error
                .unwrap()
                .contains("No terminal driver available"),
            "the null driver must say why"
        );
    }
    assert!(!null.is_available());
}

#[test]
fn both_drivers_expose_close_so_the_contract_is_not_iterm2_only() {
    // Killing the agent leaves the terminal at a shell prompt, so a purge that
    // only signalled processes freed a process but never a tab.
    let policy = SpawnPolicy::Refuse;
    let drivers: [Box<dyn TerminalDriver>; 3] = [
        Box::new(crate::term::TmuxDriver::new("cs", policy)),
        Box::new(crate::term::Iterm2Driver::new(policy)),
        Box::new(NullDriver),
    ];
    for driver in drivers {
        let result = driver.close(&SessionRef::from_tty("ttys004"));
        assert!(
            result.ok || result.error.is_some(),
            "{} returned neither an outcome nor a reason",
            driver.name()
        );
    }
}

#[test]
fn native_windows_resolves_to_no_driver_however_it_is_asked() {
    // iTerm2 is a macOS application and the tmux driver runs `/bin/sh -lc` and
    // joins panes by a Unix tty. Even handed an availability that claims both,
    // the answer is nothing — a driver that cannot work is worse than none.
    for requested in [
        TerminalDriverName::Auto,
        TerminalDriverName::Tmux,
        TerminalDriverName::Iterm2,
    ] {
        assert_eq!(
            choose_driver(requested, available(true, true, Platform::Windows)),
            None,
            "{requested:?} should resolve to nothing on Windows"
        );
    }
}

#[test]
fn the_null_driver_names_the_phase_only_where_a_driver_could_not_exist() {
    // On Unix the error already says what to install, so a second sentence
    // would be noise; where the reason is that the port has not got there yet,
    // installing tmux would not have helped and the hint says so.
    let hint = NullDriver
        .launch(&crate::term::LaunchRequest::new("/repo", "claude"))
        .hint;
    assert_eq!(hint, crate::platform::terminal_notice());
    assert_eq!(hint.is_some(), !crate::platform::TERMINAL_CONTROL);
}

#[test]
fn opening_a_target_uses_the_platforms_handler() {
    use crate::term::{open_args, OPEN_COMMAND};

    let args = open_args("https://example.com/x");
    assert_eq!(args.last().unwrap(), "https://example.com/x");
    if cfg!(target_os = "macos") {
        assert_eq!((OPEN_COMMAND, args.len()), ("open", 1));
    } else if cfg!(windows) {
        // `start`'s first argument is a window title; without an empty one it
        // reads the target as the title and opens nothing.
        assert_eq!(OPEN_COMMAND, "cmd");
        assert_eq!(args[..3], ["/C", "start", ""]);
    } else {
        assert_eq!((OPEN_COMMAND, args.len()), ("xdg-open", 1));
    }
}
