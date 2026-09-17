//! Being told to stop, without a dependency: SIGINT and SIGTERM on Unix, the
//! console control events on Windows.
//!
//! `signal(2)` is two integers and a function pointer, and std already links
//! libc everywhere this runs — declaring it here is cheaper than putting
//! another crate in the install path for it. The handler does the one thing
//! that is async-signal-safe, set a flag, and the daemon's main loop notices it
//! within its poll interval and shuts down exactly the way `daemon stop` does.
//!
//! Installed before anything slow: the daemon's first refresh scans every
//! process on the machine, and a daemon that cannot be stopped during its own
//! startup gets killed outright — which leaves the discovery file behind
//! pointing at a pid that is gone.

#[cfg(unix)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};

    static CAUGHT: AtomicBool = AtomicBool::new(false);
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    extern "C" fn catch(_signum: i32) {
        CAUGHT.store(true, Ordering::SeqCst);
    }

    pub(crate) fn install() {
        let handler = catch as extern "C" fn(i32) as usize;
        // SAFETY: registering a handler whose whole body is one atomic store.
        unsafe {
            signal(SIGINT, handler);
            signal(SIGTERM, handler);
        }
    }

    pub(crate) fn caught() -> bool {
        CAUGHT.load(Ordering::SeqCst)
    }
}

/// The same idea through the Windows console: one call, one flag.
///
/// `SetConsoleCtrlHandler` is the only way to be told about Ctrl-C, Ctrl-Break
/// and the console closing, and the handler runs on a thread of the system's
/// own — so, exactly as above, it does the one safe thing and sets a flag that
/// the main loop notices within its poll interval. Returning TRUE says the
/// event is handled, which is what stops the runtime terminating the process
/// out from under a daemon that is mid-shutdown and leaving `notify.json`
/// pointing at a pid that is gone.
///
/// `kernel32` is already on the link line via `std`, so this costs no
/// dependency — the same trade as `signal(2)` above.
#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};

    static CAUGHT: AtomicBool = AtomicBool::new(false);

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    unsafe extern "system" fn catch(_ctrl_type: u32) -> i32 {
        CAUGHT.store(true, Ordering::SeqCst);
        1
    }

    pub(crate) fn install() {
        // SAFETY: registering a handler whose whole body is one atomic store.
        unsafe {
            SetConsoleCtrlHandler(Some(catch), 1);
        }
    }

    pub(crate) fn caught() -> bool {
        CAUGHT.load(Ordering::SeqCst)
    }
}

/// Anywhere else: the daemon is still stopped by `claude-sessions daemon stop`,
/// which is an HTTP call and needs nothing from the platform.
#[cfg(not(any(unix, windows)))]
mod platform {
    pub(crate) fn install() {}
    pub(crate) fn caught() -> bool {
        false
    }
}

pub(crate) use platform::{caught, install};
