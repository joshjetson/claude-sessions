//! Where errors go while the dashboard owns the terminal.
//!
//! The dashboard draws on the alternate screen and ratatui repaints only the
//! cells its own buffer says have changed. Anything else that writes to the
//! terminal — a child process that inherited stderr, a background thread that
//! panicked — lands on top of the frame and is never painted over. The user saw
//! error text scroll the dashboard up and stay there until a restart.
//!
//! So nothing may write to the terminal while the TUI is up, and every error
//! needs somewhere else to go. This module gives it two places:
//!
//! - a file, `logs/errors.log` under the runtime directory, with one
//!   timestamped line per error, capped the same way the memory log is;
//! - a queue the event loop drains into the Notifications feed, open only while
//!   a dashboard is running. The daemon never opens it, so a report there only
//!   reaches the file.
//!
//! Every function here is safe to call from any thread and from a panic hook.
//! None of them panic, and none of them write to stdout or stderr. A failure to
//! write the log is dropped: an error logger that raises its own errors would
//! bring back the very problem it exists to remove.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, ThreadId};

use crate::paths::Paths;
use crate::util::iso_now;

/// How many reports wait for the event loop before older ones are dropped.
///
/// The loop drains the queue every tick, so this only fills when the loop has
/// stopped. A thread that panics in a tight loop must not then grow memory
/// without bound.
pub const MAX_QUEUED: usize = 100;

/// The file every error is appended to.
pub fn log_path(paths: &Paths) -> PathBuf {
    paths.logs_dir.join("errors.log")
}

/// One error, as the Notifications feed receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorReport {
    /// A short tag for what failed: `kill`, `panic`, `sound`.
    pub source: String,
    pub message: String,
}

/// Where reports go in this process. Empty until [`install`] names a file and
/// until a dashboard opens the queue, which is what keeps a unit test from
/// writing into the developer's own runtime directory.
struct Sink {
    path: Option<PathBuf>,
    label: &'static str,
    queue: Option<Vec<ErrorReport>>,
}

static SINK: Mutex<Sink> = Mutex::new(Sink {
    path: None,
    label: "",
    queue: None,
});

/// The sink, even after a thread panicked while holding it. A poisoned lock
/// here only means one report may be half-queued, and losing the logger for
/// the rest of the session would be worse.
fn sink() -> MutexGuard<'static, Sink> {
    SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The sink if nobody else holds it. The panic hook uses this: a thread that
/// panics while it holds the sink would otherwise wait on itself forever.
fn try_sink() -> Option<MutexGuard<'static, Sink>> {
    match SINK.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// Name the log file for this process. `label` says which process writes each
/// line — `dashboard` or `daemon` — because both share the one file.
pub fn install(paths: &Paths, label: &'static str) {
    let mut sink = sink();
    sink.path = Some(log_path(paths));
    sink.label = label;
}

/// Write an error to the log file only.
///
/// For failures that must never ring: a notification sound that fails would
/// otherwise raise an error notification, which plays a sound, which fails.
pub fn record(source: &str, message: &str) {
    let target = {
        let sink = sink();
        sink.path.clone().map(|path| (path, sink.label))
    };
    if let Some((path, label)) = target {
        let _ = append(&path, &format_line(&iso_now(), label, source, message));
    }
}

/// Write an error to the log file, and queue it for the Notifications feed
/// when a dashboard is running.
pub fn report(source: &str, message: &str) {
    record(source, message);
    enqueue(&mut sink(), source, message);
}

fn enqueue(sink: &mut Sink, source: &str, message: &str) {
    if let Some(queue) = sink.queue.as_mut() {
        if queue.len() >= MAX_QUEUED {
            queue.remove(0);
        }
        queue.push(ErrorReport {
            source: source.to_string(),
            message: message.to_string(),
        });
    }
}

/// Every report queued since the last call. Empty when no dashboard is running.
pub fn drain() -> Vec<ErrorReport> {
    sink()
        .queue
        .as_mut()
        .map(std::mem::take)
        .unwrap_or_default()
}

/// One log line: timestamp, process, source, and the message on one line.
///
/// Line breaks inside the message become ` | ` and blank lines go, so one error is always one line
/// and the cap below trims whole errors rather than halves of one.
pub fn format_line(ts: &str, label: &str, source: &str, message: &str) -> String {
    let flat = message
        .trim()
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    let label = if label.is_empty() { "-" } else { label };
    format!("{ts} [{label}] {source}: {flat}\n")
}

/// Append one line and trim the file once it passes the cap.
///
/// The cap and the number of lines kept are the memory log's, from
/// [`crate::diagnostics`]: the same reason applies to both, and one number is
/// easier to reason about than two.
pub fn append(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    drop(file);
    crate::diagnostics::rotate(path);
    Ok(())
}

/// The panic, as one sentence: which thread, where, and what it said.
fn describe_panic(info: &PanicHookInfo<'_>) -> String {
    let current = thread::current();
    let name = current.name().unwrap_or("<unnamed>");
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_string());
    match info.location() {
        Some(at) => format!(
            "thread '{name}' panicked at {}:{}:{}: {payload}",
            at.file(),
            at.line(),
            at.column()
        ),
        None => format!("thread '{name}' panicked: {payload}"),
    }
}

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Errors routed away from the terminal for as long as this value lives.
///
/// Created right after the dashboard takes the screen. It opens the queue and
/// replaces the panic hook, so a panic on any thread goes to the log and to the
/// feed instead of being printed over the frame. Dropping it closes the queue
/// and puts the previous hook back, so a panic after teardown prints normally.
///
/// It must be declared BEFORE the screen guard in the function that owns both.
/// Locals drop in reverse order, so the screen is released first and the
/// one-line pointer this prints after a main-thread panic lands on the user's
/// own shell rather than on the alternate screen that is about to vanish.
pub struct TuiErrors {
    previous: Option<Hook>,
    main_panic: Arc<Mutex<Option<String>>>,
    path: Option<PathBuf>,
}

impl TuiErrors {
    pub fn install() -> Self {
        let path = {
            let mut sink = sink();
            sink.queue = Some(Vec::new());
            sink.path.clone()
        };
        let previous = panic::take_hook();
        let main = thread::current().id();
        let main_panic = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&main_panic);
        panic::set_hook(Box::new(move |info| on_panic(info, main, &slot)));
        TuiErrors {
            previous: Some(previous),
            main_panic,
            path,
        }
    }
}

/// The hook body. It writes nothing to the terminal.
///
/// A background thread's panic becomes a red notification, because that thread
/// is now dead and whatever it did has stopped. The main thread's panic cannot
/// be shown in the feed, since the feed dies with it, so it is kept for
/// [`TuiErrors`] to print once the terminal is back.
fn on_panic(info: &PanicHookInfo<'_>, main: ThreadId, slot: &Mutex<Option<String>>) {
    let text = describe_panic(info);
    let Some(mut sink) = try_sink() else {
        return;
    };
    let target = sink.path.clone().map(|path| (path, sink.label));
    enqueue(&mut sink, "panic", &text);
    drop(sink);
    if let Some((path, label)) = target {
        let _ = append(&path, &format_line(&iso_now(), label, "panic", &text));
    }
    if thread::current().id() == main {
        if let Ok(mut kept) = slot.try_lock() {
            *kept = Some(text);
        }
    }
}

impl Drop for TuiErrors {
    fn drop(&mut self) {
        sink().queue = None;
        // `set_hook` panics when called from a thread that is already
        // panicking, and a second panic aborts the process. The process is on
        // its way out in that case anyway, so the hook stays.
        if !thread::panicking() {
            if let Some(previous) = self.previous.take() {
                let _ = panic::take_hook();
                panic::set_hook(previous);
            }
        }
        let crashed = self
            .main_panic
            .lock()
            .map(|mut kept| kept.take())
            .unwrap_or(None);
        if let Some(text) = crashed {
            // The terminal is restored by now (see the type's doc comment), so
            // this is the one line the crash is allowed to print.
            let log = self
                .path
                .as_ref()
                .map(|path| format!(" — logged to {}", path.display()))
                .unwrap_or_default();
            eprintln!("claude-sessions crashed: {text}{log}");
        }
    }
}

/// A panic hook for the daemon: log the panic, then run the hook that was
/// there before.
///
/// The daemon has no terminal to protect. Its stderr already goes to
/// `daemon.log`, so the previous hook keeps writing there. This only adds the
/// line to `errors.log`, so one file holds the errors of both processes.
pub fn chain_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let text = describe_panic(info);
        if let Some(sink) = try_sink() {
            let target = sink.path.clone().map(|path| (path, sink.label));
            drop(sink);
            if let Some((path, label)) = target {
                let _ = append(&path, &format_line(&iso_now(), label, "panic", &text));
            }
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests;
