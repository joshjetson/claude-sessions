//! One tick: scan, enrich, prune, group, run the watchers, emit.
//!
//! # Re-entrancy
//!
//! Concurrent calls collapse into one trailing re-run. A client pressing
//! refresh, a launch starting the fast poll and the timer itself can all land
//! inside a single tick, and running the scan three times over would only make
//! the next tick later. The gate is the Node original's three flags —
//! in-flight, queued, force — with the same outcome: at most one extra run, and
//! it sees the strongest options anybody asked for.
//!
//! # A bad tick never kills the loop
//!
//! Every tick runs inside [`catch_unwind`]. Claude Code's transcript format
//! moves between releases and the scan shells out to the operating system, so a
//! tick has real ways to fail; the next one retries a second later. A panic is
//! counted so it shows up in `stats()` rather than passing in silence.

use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use crate::hook_state::session_status;
use crate::paths::Paths;
use crate::scan::ProcessSource;
use crate::transcript::TaskRefCache;
use crate::types::{RawSession, Session, SessionStatus};
use crate::util::{activity_label, project_name, trim_trailing_separators};

use super::caches::Caches;
use super::engine::{EngineInner, ScanState};
use super::events::{sessions_event, EngineEvent};
use super::lock;
use super::state::SessionIndex;

impl<S: ProcessSource> EngineInner<S> {
    /// Rebuild the sessions list. Safe to call from anywhere, at any rate.
    pub(crate) fn refresh(&self, force_discovery: bool) {
        {
            let mut gate = lock(&self.gate);
            if gate.in_flight {
                gate.queued = true;
                gate.force = gate.force || force_discovery;
                self.counters.collapsed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            gate.in_flight = true;
        }

        let mut force = force_discovery;
        loop {
            self.guarded_tick(force);
            let mut gate = lock(&self.gate);
            if gate.queued {
                // The single trailing re-run, carrying whatever the collapsed
                // callers asked for.
                force = gate.force;
                gate.queued = false;
                gate.force = false;
                continue;
            }
            gate.in_flight = false;
            return;
        }
    }

    fn guarded_tick(&self, force_discovery: bool) {
        self.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        if catch_unwind(AssertUnwindSafe(|| self.tick(force_discovery))).is_err() {
            self.counters.panics.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn tick(&self, force_discovery: bool) {
        // One instant for the whole tick, so every session is judged against
        // the same clock and a slow scan cannot make the first row look fresher
        // than the last.
        let now = self.now();
        // Before anything reads a setting: a role or alert edit takes effect on
        // the next tick rather than at the next daemon restart.
        self.sync_config();

        // The scan guard is held across the watchers because the archive asks
        // the same transcript-head cache the pairing rules use. Lock order is
        // scan -> state throughout (see the module docs on `daemon`).
        let mut scan = self.scan();
        let ScanState { scanner, caches } = &mut *scan;

        let raw = scanner.scan_sessions(now);
        let complete = scanner.last_scan_complete();
        let live_files: HashSet<PathBuf> =
            raw.iter().filter_map(|s| s.session_file.clone()).collect();

        let refs = scanner.task_refs();
        // The sessions a hook has spoken for. The blocked-prompt watcher needs
        // it: with hooks installed, a pending tool call is a running tool
        // unless a hook said otherwise, so the transcript-only guess is off.
        let mut hooked: HashSet<String> = HashSet::new();
        let sessions: Vec<Session> = raw
            .into_iter()
            .map(|session| {
                let (session, has_hook) = enrich(session, caches, refs, &self.paths, now);
                if has_hook {
                    hooked.insert(session.session_id.clone());
                }
                session
            })
            .collect();

        // Drop the cursor, the compacting answer and the retry timer for every
        // transcript that is no longer live. Dropping the cursor is what closes
        // the file.
        caches.prune(&live_files);

        let discovered = self.discovered_dirs(caches, force_discovery, now);
        let sessions = SessionIndex::build(sessions, |session| project_name(&session.cwd));

        // Node's order, kept: the stall watcher reads the links as they were
        // before this tick's linking, so a session linked a moment ago is not
        // immediately judged for silence.
        self.notify_awaiting_decisions(&sessions, &hooked, now);
        self.notify_stalled_sessions(&sessions, now);
        self.update_quiet_sessions(&sessions, now);
        self.auto_archive_vanished_sessions(&sessions, scanner.task_refs());
        self.link_pending_sessions(&sessions, now);
        drop(scan);

        let event = {
            let mut state = self.state();
            state.sessions = sessions;
            state.sessions_complete = complete;
            state.discovered_dirs = discovered;
            sessions_event(&state)
        };
        self.publish(EngineEvent::Sessions(Box::new(event)));
    }

    /// The repository folders under each configured group, for the tree's
    /// inactive rows.
    fn discovered_dirs(
        &self,
        caches: &mut Caches,
        force: bool,
        now: SystemTime,
    ) -> std::collections::BTreeMap<String, Vec<String>> {
        let config = self.config();
        config
            .groups()
            .iter()
            .map(|group| {
                let path = trim_trailing_separators(&group.path).to_string();
                let dirs = caches.discover(std::path::Path::new(&path), force, now);
                (path, dirs)
            })
            .collect()
    }
}

/// A scanned session plus everything its transcript and its hook state say.
///
/// The transcript is read through the cursor cache, so this costs the bytes
/// that were appended since the last tick rather than the 512 KB tail the Node
/// app re-parsed every second for every open session. The second value is
/// whether a hook state file exists for the session.
fn enrich(
    raw: RawSession,
    caches: &mut Caches,
    refs: &mut TaskRefCache,
    paths: &Paths,
    now: SystemTime,
) -> (Session, bool) {
    let parsed = raw
        .session_file
        .as_deref()
        .and_then(|path| caches.session(path));
    let compacting = raw
        .session_file
        .as_deref()
        .is_some_and(|path| caches.compacting(path, now));
    let task_id = raw
        .session_file
        .as_deref()
        .and_then(|path| caches.task_id(path, refs, now));

    // The transcript's own session id wins: it is what `claude --resume` takes,
    // and the process-derived one is a file name.
    let session_id = parsed
        .as_ref()
        .map(|p| p.session_id.clone())
        .filter(|id| !id.is_empty())
        .unwrap_or(raw.session_id);

    // A hook names the session by its transcript's file stem. That is normally
    // the transcript's own id too, and both are tried in case they differ.
    let stem = raw
        .session_file
        .as_deref()
        .and_then(|path| path.file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    let hook = caches.hooks.lookup(paths, &[&stem, &session_id]);

    let last_entry = parsed.as_ref().and_then(|p| p.last_entry.clone());
    let status = match (compacting, raw.status) {
        (true, _) => SessionStatus::Compacting,
        // A placeholder keeps the status the scanner gave it. Node recomputed
        // here and turned every `starting-<pid>` row into "idle", because a
        // process with no transcript yet has no trailing entry to judge.
        (false, Some(status)) => status,
        // Aged from when the CONVERSATION last moved, never from bookkeeping
        // writes, and combined with the hook state. The embedded scan calls the
        // same function, so the two paths cannot disagree.
        (false, None) => session_status(last_entry.as_ref(), raw.session_mtime, hook.as_ref(), now),
    };
    let activity_detail = if compacting {
        "compacting".to_string()
    } else {
        activity_label(last_entry.as_ref())
    };

    let session = Session {
        session_id,
        pids: raw.pids,
        cwd: raw.cwd,
        tty: raw.tty,
        lstart: raw.lstart,
        run_id: raw.run_id,
        session_file: raw.session_file,
        session_mtime: raw.session_mtime,
        session_size: raw.session_size,
        status,
        activity_detail,
        starting: raw.starting,
        git_branch: parsed
            .as_ref()
            .map(|p| p.git_branch.clone())
            .filter(|branch| !branch.is_empty()),
        last_timestamp: parsed
            .as_ref()
            .map(|p| p.last_timestamp.clone())
            .filter(|stamp| !stamp.is_empty()),
        last_usage: parsed.as_ref().and_then(|p| p.last_usage),
        last_entry,
        cumulative_usage: parsed.as_ref().map(|p| p.cumulative_usage),
        prompts: parsed.map(|p| p.prompts).unwrap_or_default(),
        task_id,
    };
    (session, hook.is_some())
}
