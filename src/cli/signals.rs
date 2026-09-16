//! SIGINT and SIGTERM, without a dependency.
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

#[cfg(not(unix))]
mod platform {
    pub(crate) fn install() {}
    pub(crate) fn caught() -> bool {
        false
    }
}

pub(crate) use platform::{caught, install};
