//! What Claude Code's own hooks say a session is doing, and how that combines
//! with what the transcript says.
//!
//! # Why hooks at all
//!
//! The transcript cannot tell a permission prompt from a slow tool. Both are
//! an assistant tool call with no result yet, and the prompt itself is a modal
//! in Claude Code's UI that never reaches the file. Claude Code does announce
//! it to hooks (`PermissionRequest`, and `Notification` with
//! `notification_type: "permission_prompt"`), so a hook that records the latest
//! event per session makes the status exact where the transcript can only
//! guess.
//!
//! # The pieces
//!
//! - `claude-sessions hook` ([`run`]) reads one hook payload from stdin and
//!   records it in `<runtime_dir>/hooks/<session_id>.json`, or removes that
//!   file on `SessionEnd`. It prints nothing and always exits 0: the stdout of
//!   a `SessionStart` or `UserPromptSubmit` hook is added to Claude's context,
//!   and a failing hook shows an error in the person's session.
//! - [`HookStateCache`] reads those files once per change, for the scan loops.
//! - [`session_status`] combines the hook state with the transcript's status.
//!   The newer of the two wins. A tie goes to the hook.
//!
//! Everything is optional. With no hooks installed there are no files, and the
//! status is the transcript's alone.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::Paths;
use crate::types::{LastEntry, SessionStatus};
use crate::util::{activity_time, detect_session_status, parse_timestamp};

/// The directory under the runtime directory that holds one file per session.
pub const HOOKS_DIR: &str = "hooks";

/// Where a hook invocation that failed leaves one line. Nothing else in the
/// hook path writes anywhere a person would see it.
const ERROR_LOG: &str = "errors.log";

/// The error log starts over past this size, so a hook that fails on every
/// tool call cannot fill the disk.
const ERROR_LOG_CAP: u64 = 64 * 1024;

/// A state file this old belongs to a session that ended without a
/// `SessionEnd` hook (a crash, a killed terminal). `SessionStart` sweeps them.
const STALE_STATE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The tools that hand control back to the person on purpose.
const DECISION_TOOLS: &[&str] = &["AskUserQuestion", "ExitPlanMode"];

/// The latest hook event recorded for one session: the file format of
/// `<runtime_dir>/hooks/<session_id>.json`.
///
/// It stores the raw facts, not a status. [`HookState::status`] maps them, so
/// a change to the mapping applies to files already on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookState {
    pub session_id: String,
    /// `hook_event_name` as Claude Code sent it, for example `PreToolUse`.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Set on a `Notification` event only, for example `permission_prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    /// When the hook ran, in the same ISO-8601 form the transcripts use.
    pub timestamp: String,
}

impl HookState {
    /// The status this event means, or `None` when it means nothing to the
    /// status. See [`status_for_event`].
    pub fn status(&self) -> Option<SessionStatus> {
        status_for_event(
            &self.event,
            self.tool_name.as_deref(),
            self.notification_type.as_deref(),
        )
    }

    /// [`Self::timestamp`] as an instant.
    pub fn instant(&self) -> Option<SystemTime> {
        parse_timestamp(&self.timestamp).map(SystemTime::from)
    }
}

/// What one hook event says about the session.
///
/// | Event                                             | Status   |
/// |---------------------------------------------------|----------|
/// | `UserPromptSubmit`, `PostToolUse`, `SubagentStop` | working  |
/// | `PreToolUse`                                      | working  |
/// | `PreToolUse` for `AskUserQuestion` / `ExitPlanMode` | awaiting |
/// | `PermissionRequest`                               | awaiting |
/// | `Notification` `permission_prompt` / `elicitation_dialog` / `elicitation_url_dialog` / `agent_needs_input` | awaiting |
/// | `Notification` `elicitation_complete` / `elicitation_response` | working |
/// | `Notification` `idle_prompt`                      | idle     |
/// | `Stop`, `StopFailure`, `SessionStart`             | idle     |
/// | anything else                                     | no say   |
///
/// A question tool is awaiting from its `PreToolUse` on. Mapping it to
/// working would let the hook, which is newer than the transcript line,
/// overrule the transcript's correct "awaiting" for as long as the question
/// stays open.
pub fn status_for_event(
    event: &str,
    tool_name: Option<&str>,
    notification_type: Option<&str>,
) -> Option<SessionStatus> {
    match event {
        "UserPromptSubmit" | "PostToolUse" | "SubagentStop" => Some(SessionStatus::Working),
        "PreToolUse" => Some(
            if tool_name.is_some_and(|tool| DECISION_TOOLS.contains(&tool)) {
                SessionStatus::Awaiting
            } else {
                SessionStatus::Working
            },
        ),
        "PermissionRequest" => Some(SessionStatus::Awaiting),
        "Notification" => match notification_type {
            Some(
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input",
            ) => Some(SessionStatus::Awaiting),
            // The user answered the dialog, so Claude has the turn again.
            Some("elicitation_complete" | "elicitation_response") => Some(SessionStatus::Working),
            Some("idle_prompt") => Some(SessionStatus::Idle),
            _ => None,
        },
        "Stop" | "StopFailure" | "SessionStart" => Some(SessionStatus::Idle),
        _ => None,
    }
}

/// What the hook command does with one payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Replace the session's state file with this state.
    Record(HookState),
    /// The session ended. Delete its state file.
    Remove { session_id: String },
    /// The payload says nothing about the status, or it is unreadable.
    Ignore,
}

/// Read one hook payload, as Claude Code writes it to the hook's stdin.
///
/// Read field by field from a loose JSON value rather than through a strict
/// struct. The payload gains fields between releases, and a field whose type
/// changed must cost that one field, never the whole event.
pub fn parse_hook_input(input: &[u8], now: DateTime<Utc>) -> HookAction {
    let Ok(value) = serde_json::from_slice::<Value>(input) else {
        return HookAction::Ignore;
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (Some(event), Some(session_id)) = (text("hook_event_name"), text("session_id")) else {
        return HookAction::Ignore;
    };
    if !is_safe_session_id(&session_id) {
        return HookAction::Ignore;
    }
    if event == "SessionEnd" {
        return HookAction::Remove { session_id };
    }
    let state = HookState {
        session_id,
        event,
        tool_name: text("tool_name"),
        notification_type: text("notification_type"),
        timestamp: now.to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    // An event with no say is not recorded at all. Recording it would replace
    // a state that does have a say, and the session would lose it.
    if state.status().is_none() {
        return HookAction::Ignore;
    }
    HookAction::Record(state)
}

/// The id becomes a file name, so only the characters a Claude Code session
/// id uses are accepted. Anything else could name a path outside the hooks
/// directory.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<runtime_dir>/hooks`.
pub fn hooks_dir(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join(HOOKS_DIR)
}

/// `<runtime_dir>/hooks/<session_id>.json`, or `None` for an id that is not
/// safe to use as a file name.
pub fn state_path(paths: &Paths, session_id: &str) -> Option<PathBuf> {
    is_safe_session_id(session_id).then(|| hooks_dir(paths).join(format!("{session_id}.json")))
}

/// Carry out one payload's action.
pub fn apply(paths: &Paths, action: &HookAction) -> io::Result<()> {
    match action {
        HookAction::Ignore => Ok(()),
        HookAction::Remove { session_id } => {
            let Some(path) = state_path(paths, session_id) else {
                return Ok(());
            };
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        }
        HookAction::Record(state) => {
            let Some(path) = state_path(paths, &state.session_id) else {
                return Ok(());
            };
            let dir = hooks_dir(paths);
            fs::create_dir_all(&dir)?;
            if state.event == "SessionStart" {
                sweep_stale(&dir, SystemTime::now());
            }
            write_atomic(&path, &serde_json::to_vec(state).map_err(io::Error::other)?)
        }
    }
}

/// Write through a temporary file in the same directory, then rename it into
/// place. A scan that reads the file mid-write then sees the old state or the
/// new one, never half of either. The temporary name carries the process id
/// because two hooks for one session can run at the same moment.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("state.json");
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Delete state files (and leftover temporary files) older than
/// [`STALE_STATE`]. Best effort: a file that cannot be read or removed is left
/// for the next sweep.
fn sweep_stale(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_state = path
            .extension()
            .is_some_and(|ext| ext == "json" || ext == "tmp");
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|mtime| now.duration_since(mtime).unwrap_or_default() >= STALE_STATE);
        if is_state && old {
            let _ = fs::remove_file(path);
        }
    }
}

/// Read one state file. `None` when it is missing or unreadable.
pub fn read_state(path: &Path) -> Option<HookState> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `claude-sessions hook`: record one hook payload from stdin.
///
/// Prints nothing, whatever happens. The caller exits 0 afterwards. A failure
/// goes to `<runtime_dir>/hooks/errors.log`, and only when that directory can
/// be written.
pub fn run(paths: &Paths) {
    let mut input = Vec::new();
    let outcome = io::stdin()
        .lock()
        .read_to_end(&mut input)
        .and_then(|_| apply(paths, &parse_hook_input(&input, Utc::now())));
    if let Err(error) = outcome {
        log_error(paths, &error);
    }
}

fn log_error(paths: &Paths, error: &io::Error) {
    let path = hooks_dir(paths).join(ERROR_LOG);
    let append = fs::metadata(&path).is_ok_and(|meta| meta.len() < ERROR_LOG_CAP);
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&path);
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{} {error}", crate::util::iso_now());
    }
}

/// The hook state files, each read once per change.
///
/// A scan tick asks for every live session once a second. The cache costs one
/// `stat` per session per tick, and one small read when the file changed.
#[derive(Debug, Default)]
pub struct HookStateCache {
    entries: HashMap<PathBuf, Cached>,
    /// Paths asked about since the last [`Self::sweep`].
    seen: HashSet<PathBuf>,
}

#[derive(Debug)]
struct Cached {
    mtime: SystemTime,
    len: u64,
    state: Option<HookState>,
}

impl HookStateCache {
    pub fn new() -> Self {
        HookStateCache::default()
    }

    /// The hook state for a session, tried under each candidate id in order.
    ///
    /// The hook payload names a session by its transcript's file stem. The
    /// scan knows a session by the id its transcript carries, which is
    /// normally the same. Both are passed so a mismatch still finds the file.
    pub fn lookup(&mut self, paths: &Paths, candidates: &[&str]) -> Option<HookState> {
        for id in candidates {
            let Some(path) = state_path(paths, id) else {
                continue;
            };
            self.seen.insert(path.clone());
            let Ok(meta) = fs::metadata(&path) else {
                self.entries.remove(&path);
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let len = meta.len();
            let fresh = self
                .entries
                .get(&path)
                .is_some_and(|cached| cached.mtime == mtime && cached.len == len);
            if !fresh {
                let state = read_state(&path);
                self.entries
                    .insert(path.clone(), Cached { mtime, len, state });
            }
            if let Some(state) = self.entries.get(&path).and_then(|c| c.state.clone()) {
                return Some(state);
            }
        }
        None
    }

    /// Forget every file not asked about since the last sweep, so a session
    /// that went away does not keep its entry for the life of the process.
    pub fn sweep(&mut self) {
        let seen = std::mem::take(&mut self.seen);
        self.entries.retain(|path, _| seen.contains(path));
    }
}

/// Combine the transcript's status with the hook state.
///
/// The hook wins when it is at least as new as the transcript's newest
/// conversational line, and when the event has a say. Otherwise the transcript
/// wins. That rule is what clears a stale hook state without any hook firing:
///
/// - A denied permission prompt fires no `PostToolUse`. The rejection and the
///   interrupt marker land in the transcript after the `PermissionRequest`, so
///   the transcript's "idle" wins.
/// - An answered question lands as a tool result after the `PreToolUse`, so
///   the transcript's "working" wins until `PostToolUse` arrives.
///
/// The transcript side is its conversational activity only, never the file's
/// mtime. Bookkeeping lines (the hook results themselves among them) touch the
/// mtime right after every hook, and would otherwise hide every hook state.
pub fn fuse(
    transcript: SessionStatus,
    transcript_at: Option<SystemTime>,
    hook: Option<&HookState>,
) -> SessionStatus {
    let Some(hook) = hook else {
        return transcript;
    };
    let (Some(status), Some(hook_at)) = (hook.status(), hook.instant()) else {
        return transcript;
    };
    match transcript_at {
        Some(at) if hook_at < at => transcript,
        _ => status,
    }
}

/// A session's status from everything known about it: the transcript's newest
/// conversational entry, the file's mtime as the fallback clock, and the hook
/// state when the hooks are installed.
///
/// The daemon and the embedded scan both call this, so they cannot disagree.
/// Compacting and starting are decided by the callers before this, and keep
/// their precedence.
pub fn session_status(
    last_entry: Option<&LastEntry>,
    session_mtime: SystemTime,
    hook: Option<&HookState>,
    now: SystemTime,
) -> SessionStatus {
    let transcript =
        detect_session_status(last_entry, activity_time(last_entry, session_mtime), now);
    fuse(
        transcript,
        last_entry.and_then(LastEntry::activity_instant),
        hook,
    )
}

#[cfg(test)]
mod tests;
