//! Memory instrumentation, kept light.
//!
//! Ported from the Node app's `src/diagnostics.js`, which existed because the
//! dashboard once aborted with a V8 heap OOM and the stack trace alone did not
//! say what had grown. A Rust build has no V8 heap to leak, so the elaborate
//! half — dumping collection sizes past a threshold, counting buffered
//! performance measures — does not carry over. What does carry over is the
//! cheap trend line: one sample a minute, one appended line, a capped file.
//!
//! Off unless asked for. It is a diagnostic, not a feature: enable it with
//! `CLAUDE_SESSIONS_DIAGNOSTICS=1` or `"diagnostics": true` in the config, and
//! both the daemon and the dashboard will install one.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::paths::Paths;
use crate::util::iso_now;

/// Trim rather than letting the diagnostic become the problem.
pub const MAX_LOG_BYTES: u64 = 256 * 1024;
/// Lines kept when the log is trimmed.
pub const KEEP_LINES: usize = 500;
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

pub fn memory_log(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join("memory.log")
}

/// A running watcher. Dropping it stops the thread at its next wake.
pub struct MemoryWatch {
    running: Arc<AtomicBool>,
}

impl Drop for MemoryWatch {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// Start sampling, if diagnostics are enabled. `label` says which process this
/// is — `dashboard` or `daemon` — so one log can hold both.
pub fn start(paths: &Paths, label: &'static str, enabled: bool) -> Option<MemoryWatch> {
    if !enabled {
        return None;
    }
    let path = memory_log(paths);
    let runtime_dir = paths.runtime_dir.clone();
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    // One sample immediately, so an install is visible in the log without
    // waiting a minute for the first tick.
    sample(&runtime_dir, &path, label);
    thread::Builder::new()
        .name("claude-sessions-memory".into())
        .spawn(move || {
            // Woken often, sampling rarely: a dropped watch must not keep the
            // process alive for the rest of a minute.
            while flag.load(Ordering::SeqCst) {
                for _ in 0..60 {
                    thread::sleep(Duration::from_secs(1));
                    if !flag.load(Ordering::SeqCst) {
                        return;
                    }
                }
                sample(&runtime_dir, &path, label);
            }
        })
        .ok()?;
    Some(MemoryWatch { running })
}

/// One line: timestamp, which process, and the memory figure this platform can
/// give cheaply.
pub fn sample(runtime_dir: &Path, path: &Path, label: &str) {
    let Some(reading) = memory_reading() else {
        return;
    };
    let line = format!(
        "{} {label} {}={}MB\n",
        iso_now(),
        reading.kind,
        reading.megabytes
    );
    let _ = fs::create_dir_all(runtime_dir);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
    rotate(path);
}

/// Keep the last [`KEEP_LINES`] once the file passes [`MAX_LOG_BYTES`].
pub fn rotate(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let Ok(body) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    let kept = lines.split_at(lines.len().saturating_sub(KEEP_LINES)).1;
    let mut trimmed = kept.join("\n");
    trimmed.push('\n');
    let _ = fs::write(path, trimmed);
}

/// What this platform can tell us about our own memory, cheaply and without a
/// child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReading {
    /// `rss` where the figure is current, `peak` where only the high-water mark
    /// is available. Named in the log so a reader is never misled.
    pub kind: &'static str,
    pub megabytes: u64,
}

pub fn memory_reading() -> Option<MemoryReading> {
    platform::reading()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::MemoryReading;

    /// `/proc/self/statm` field 2 is resident pages.
    pub(super) fn reading() -> Option<MemoryReading> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(MemoryReading {
            kind: "rss",
            megabytes: pages.saturating_mul(page_size()) / (1024 * 1024),
        })
    }

    fn page_size() -> u64 {
        // SAFETY: sysconf takes an int and returns a long; _SC_PAGESIZE is 30
        // on Linux. A failure returns -1, which the clamp below rejects.
        let size = unsafe { sysconf(30) };
        if size > 0 {
            size as u64
        } else {
            4096
        }
    }

    extern "C" {
        fn sysconf(name: i32) -> i64;
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::MemoryReading;

    /// The first two fields of `struct rusage` are `timeval`s and the third is
    /// `ru_maxrss`; only that third field is read. Both are 16 bytes on the
    /// platforms this builds for, so the prefix below is layout-compatible.
    #[repr(C)]
    struct Rusage {
        utime: [i64; 2],
        stime: [i64; 2],
        maxrss: i64,
        rest: [i64; 14],
    }

    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }

    /// macOS reports `ru_maxrss` in bytes, and it is the high-water mark rather
    /// than the current figure — which is why the log names it `peak`. Reading
    /// the live resident size would mean a mach call or a `ps` child, and a
    /// diagnostic is not worth either.
    pub(super) fn reading() -> Option<MemoryReading> {
        let mut usage = Rusage {
            utime: [0; 2],
            stime: [0; 2],
            maxrss: 0,
            rest: [0; 14],
        };
        // SAFETY: a stack struct at least as large as the real one, filled by
        // the kernel; RUSAGE_SELF is 0.
        if unsafe { getrusage(0, &mut usage) } != 0 {
            return None;
        }
        Some(MemoryReading {
            kind: "peak",
            megabytes: (usage.maxrss.max(0) as u64) / (1024 * 1024),
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::MemoryReading;

    pub(super) fn reading() -> Option<MemoryReading> {
        None
    }
}

#[cfg(test)]
mod tests;
