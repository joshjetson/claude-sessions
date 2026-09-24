//! Taking the terminal, the event loop, and giving the terminal back.
//!
//! Ported from the Node app's `src/ui.js` (alternate-screen mount plus
//! `suspendUI`) and the loop half of `src/index.js`.
//!
//! Two departures worth naming. Node coalesced frames at 33ms and skipped any
//! whose visible data hashed the same as the last one, because re-rendering an
//! Ink tree at a steady cadence for hours leaked V8 heap. Brief §9 deletes that
//! machinery: ratatui draws from state into a buffer and diffs it against the
//! last one, so the loop simply draws when something changed. And Node watched
//! the selected transcript with `fs.watch`; here a [`TranscriptCursor`] is
//! polled on the tick instead — O(bytes appended) either way, one fewer moving
//! part, and no platform-specific watcher to get wrong.

use std::io::{self, IsTerminal, Stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::ConfigHandle;
use crate::daemon::{client, protocol};
use crate::paths::Paths;
use crate::term::{driver_or_null, resolve_editor_from_env, run_editor, SpawnPolicy};
use crate::transcript::{Collect, TranscriptCursor};
use crate::ui::actions::{ActionResult, ActionWorker, BoardServices};
use crate::ui::app::{body_area, draw};
use crate::ui::conversation::ConversationMeta;
use crate::ui::feed::FeedEvent;
use crate::ui::feed_remote::RemoteFeed;
use crate::ui::keys::handle_key;
use crate::ui::state::{Action, AppState, Quit};
use crate::ui::transport::FeedHandle;

/// How long the loop waits for a key before doing its periodic work.
pub const TICK: Duration = Duration::from_millis(100);

/// Owning the alternate screen and raw mode, as a thing that can be released
/// and re-acquired.
///
/// A trait rather than two functions so the suspend/restore dance is testable:
/// [`RecordingScreen`] records the transitions and `suspend` can be exercised
/// without a terminal.
pub trait ScreenControl {
    fn acquire(&mut self) -> io::Result<()>;
    fn release(&mut self) -> io::Result<()>;
    fn is_acquired(&self) -> bool;
}

/// Give the terminal to `body` — an editor, a pager, anything interactive — and
/// take it back however it behaves.
///
/// Node unmounted Ink entirely for the duration: leaving it mounted meant it
/// kept consuming stdin and repainting over the child. The equivalent here is
/// leaving the alternate screen and dropping raw mode, and the `finally` is a
/// plain sequential re-acquire because `body` cannot unwind past it.
pub fn suspend<S: ScreenControl, T>(screen: &mut S, body: impl FnOnce() -> T) -> io::Result<T> {
    screen.release()?;
    let result = body();
    screen.acquire()?;
    Ok(result)
}

/// The real screen.
pub struct CrosstermScreen {
    acquired: bool,
}

impl CrosstermScreen {
    pub fn new() -> Self {
        CrosstermScreen { acquired: false }
    }
}

impl Default for CrosstermScreen {
    fn default() -> Self {
        CrosstermScreen::new()
    }
}

impl ScreenControl for CrosstermScreen {
    fn acquire(&mut self) -> io::Result<()> {
        if self.acquired {
            return Ok(());
        }
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        self.acquired = true;
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        if !self.acquired {
            return Ok(());
        }
        // Order matters: leave the alternate screen first so the cursor is
        // restored onto the user's own scrollback, then stop eating keys.
        execute!(io::stdout(), LeaveAlternateScreen)?;
        disable_raw_mode()?;
        io::stdout().flush()?;
        self.acquired = false;
        Ok(())
    }

    fn is_acquired(&self) -> bool {
        self.acquired
    }
}

impl Drop for CrosstermScreen {
    fn drop(&mut self) {
        // The terminal is left usable even on a panic: a dashboard that crashes
        // in raw mode leaves the user with an unusable shell.
        let _ = self.release();
    }
}

/// A [`ScreenControl`] that touches nothing and remembers what it was asked to
/// do.
#[derive(Debug, Default)]
pub struct RecordingScreen {
    pub acquired: bool,
    pub transitions: Vec<&'static str>,
}

impl ScreenControl for RecordingScreen {
    fn acquire(&mut self) -> io::Result<()> {
        self.acquired = true;
        self.transitions.push("acquire");
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        self.acquired = false;
        self.transitions.push("release");
        Ok(())
    }

    fn is_acquired(&self) -> bool {
        self.acquired
    }
}

/// Start the dashboard. Returns when the user quits.
pub fn run_dashboard(paths: Paths, config: ConfigHandle, policy: SpawnPolicy) -> Result<()> {
    if !io::stdout().is_terminal() {
        bail!(
            "claude-sessions needs a terminal. Run it from an interactive shell, \
             or use `claude-sessions daemon` for a headless one."
        );
    }

    // First, so an error raised while the feed is still being opened — a
    // daemon log that cannot be written, say — already has a file to go to.
    crate::errorlog::install(&paths, "dashboard");

    let services = BoardServices::new(
        paths.clone(),
        &config,
        odoo_client(&config),
        crate::optics::OpticsClient::from_config(&config).map(std::sync::Arc::new),
        policy,
    );
    // The UI thread reads this cache while labelling the QA menu row and never
    // fills it; the worker fills it. See [`crate::qaden::HeadCache`].
    let qa_heads = std::sync::Arc::clone(&services.qa_heads);
    // One sample a minute into `runtime/memory.log`, and only when asked for.
    let _diagnostics = crate::diagnostics::start(&paths, "dashboard", config.diagnostics());
    let usage_interval = config.usage().interval;
    let worker = ActionWorker::start(driver_or_null(&config, policy), policy, services);
    let (feed, notice) = open_feed(&paths, &config);
    let mut state = AppState::new(paths.clone(), config);
    state.qa_heads = qa_heads;
    // Before the first frame, so a dashboard that could not reach its daemon
    // says so on the screen it opens with rather than looking merely quiet.
    if let Some(notice) = notice {
        state.note_feed(notice);
    }
    refresh_transcripts_notice(&mut state);
    // A dashboard that OPENS on the board fetches it now.
    //
    // Switching to the board fills an empty one, but starting on it never went
    // through that path, so the tab sat empty until the 45-second poll or an
    // `r`. The symptom read as a broken board rather than an unfetched one.
    if state.view == crate::ui::state::View::Board && state.board.board.is_none() {
        state.board.loading = true;
        crate::ui::board::keys::refresh(&mut state);
    }

    // Declared BEFORE the screen on purpose. Locals drop in reverse order, so
    // on a panic the screen is released first and only then does this print
    // its one line about the crash — onto the user's shell, not onto an
    // alternate screen that is about to disappear. See [`TuiErrors`].
    //
    // [`TuiErrors`]: crate::errorlog::TuiErrors
    let _errors = crate::errorlog::TuiErrors::install();
    let mut screen = CrosstermScreen::new();
    screen.acquire()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let result = event_loop(
        &mut terminal,
        &mut screen,
        &mut state,
        feed,
        &worker,
        policy,
        usage_interval,
    );

    screen.release()?;
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Tui,
    screen: &mut CrosstermScreen,
    state: &mut AppState,
    mut feed: FeedHandle,
    worker: &ActionWorker,
    policy: SpawnPolicy,
    usage_interval: Option<Duration>,
) -> Result<()> {
    let mut conversation: Option<TranscriptCursor> = None;
    let mut last_clock = Instant::now();
    let mut role_stamp = config_stamp(state.config.path());
    // `None` is the default and means "only when `u` is pressed": the check is
    // itself a request against the quota it reports.
    let mut next_usage = usage_interval.map(|interval| Instant::now() + interval);

    loop {
        // The status bar names the transport and how stale it is, so an empty
        // pane can always be told apart from a feed that has stopped talking.
        let status = feed.status();
        state.dirty |= state.feed != status;
        state.feed = status;

        if state.dirty {
            terminal.draw(|frame| draw(frame, state))?;
            state.dirty = false;
        }

        if event::poll(TICK)? {
            let area = terminal.size()?;
            let body = body_area(
                ratatui::layout::Rect::new(0, 0, area.width, area.height),
                state,
            );
            match event::read()? {
                Event::Key(key) => handle_key(state, key, body),
                Event::Resize(..) => state.dirty = true,
                _ => {}
            }
        }

        // A remote feed that has said nothing at all since it connected is
        // not a quiet machine, it is a broken transport: take the scan back
        // and say why.
        if let Some(notice) = feed.fall_back_if_silent(&state.paths, group_paths(&state.config)) {
            state.note_feed(notice);
        }

        for event in feed.drain() {
            match event {
                FeedEvent::Sessions {
                    by_project,
                    discovered,
                    scan_complete,
                } => {
                    state.discovered_dirs = discovered;
                    let empty = by_project.is_empty();
                    state.apply_sessions(by_project, scan_complete);
                    if state.selected_session_file.is_none() {
                        conversation = None;
                    }
                    sync_selected_meta(state);
                    // Only when there is nothing to show: the answer costs a
                    // `stat` and it is only ever read by the empty pane.
                    if empty {
                        refresh_transcripts_notice(state);
                    }
                }
                FeedEvent::Notification(notification) => state.push_notification(*notification),
                FeedEvent::Notifications(list) => state.replace_notifications(list),
                FeedEvent::NotificationUpdated(notification) => {
                    state.upsert_notification(*notification)
                }
                FeedEvent::NotificationsChanged {
                    ids,
                    status,
                    removed,
                } => state.apply_notifications_changed(&ids, status, removed),
                FeedEvent::Board(update) => state.apply_board(*update),
                FeedEvent::Usage(usage) => state.apply_usage(*usage),
                FeedEvent::Deploy(update) => state.apply_deploy(*update),
                FeedEvent::DeployRun(run) => {
                    let project = run.project.clone();
                    state.deploy.set_run(*run);
                    crate::ui::deploy::redraw(state, &project);
                }
                FeedEvent::DeployOutput { project, line } => {
                    state.deploy.push_line(&project, line);
                    crate::ui::deploy::redraw(state, &project);
                }
                FeedEvent::Flash(message) => state.flash(message),
            }
        }

        // Errors from any thread — a failed `kill`, a panicked worker — reach
        // the screen only through here, as red rows in the Notifications feed.
        // Nothing else may write to the terminal while this loop owns it.
        for report in crate::errorlog::drain() {
            state.push_error(report);
        }

        for result in worker.drain() {
            match result {
                ActionResult::Flash(message) => state.flash(message),
                ActionResult::Refresh => feed.feed().request_refresh(),
                ActionResult::Launched => {
                    feed.feed().note_launch();
                    state.dirty = true;
                }
                ActionResult::Usage(usage) => state.apply_usage(*usage),
                other => crate::ui::board::apply_result(state, other),
            }
        }

        // One cursor for the selected transcript, polled on the tick: only the
        // bytes appended since the last poll are parsed.
        if let Some(cursor) = conversation.as_mut() {
            if cursor.poll().map(|p| p.changed()).unwrap_or(false) {
                state.conv.messages = cursor.messages().to_vec();
                sync_selected_meta(state);
                state.dirty = true;
            }
        }

        if let (Some(at), Some(interval)) = (next_usage, usage_interval) {
            if Instant::now() >= at {
                next_usage = Some(Instant::now() + interval);
                crate::ui::keys::refresh_usage(state);
            }
        }

        for action in state.take_actions() {
            match action {
                Action::Refresh | Action::RefreshBurst => {
                    // The group list can have changed since the last scan (`a`
                    // and `d` edit it), and the scan thread owns its own copy —
                    // so a refresh re-syncs it. `set_groups` scans immediately.
                    feed.feed().set_groups(group_paths(&state.config));
                }
                Action::SelectSession { session_file, .. } => {
                    conversation = open_conversation(state, session_file);
                    state.dirty = true;
                }
                Action::LaunchSession { .. } => {
                    feed.feed().note_launch();
                    worker.submit(action);
                }
                // Deploys belong to the engine, not to the worker: the child
                // has to outlive this dashboard.
                Action::StartDeploy { project } => {
                    if let Some(message) = feed.feed().start_deploy(&project) {
                        state.flash(message);
                    }
                }
                Action::CancelDeploy { project } => feed.feed().cancel_deploy(&project),
                Action::OpenEditor { path, line } => {
                    // The only action the main thread runs itself: it has to
                    // hand over the terminal, which the worker cannot do.
                    let editor = resolve_editor_from_env();
                    let outcome = suspend(screen, || run_editor(&editor, &path, line, policy))?;
                    terminal.clear()?;
                    state.dirty = true;
                    if let Some(error) = outcome.error {
                        state.flash(error);
                    }
                }
                // The daemon owns the usage hook when there is one, so the
                // check runs once however many dashboards are attached.
                Action::RefreshUsage if feed.feed().refresh_usage() => {}
                // The daemon owns the notification list when there is one.
                // Without one, the worker writes the change to SQLite.
                Action::Notifications { ref ids, status }
                    if feed.feed().update_notifications(ids.clone(), status) => {}
                Action::ClearNotifications if feed.feed().clear_notifications() => {}
                other => {
                    // A task launch has to be registered with the pending queue
                    // BEFORE the terminal opens, or nothing will claim the
                    // session it starts.
                    crate::ui::board::note_launch(feed.feed(), &other);
                    worker.submit(other);
                }
            }
        }

        // The status bar carries a clock, so one redraw a second regardless.
        if last_clock.elapsed() >= Duration::from_secs(1) {
            last_clock = Instant::now();
            state.dirty = true;
            refresh_role(state, &mut role_stamp);
        }

        if let Some(quit) = state.quit {
            // Plain `q` detaches and leaves the daemon working — that is the
            // point of it. Shift-Q stops the daemon too, so nothing is left
            // running in the background.
            if quit == Quit::ShutdownAll {
                feed.feed().shutdown_daemon();
            }
            return Ok(());
        }
    }
}

/// The config file's modification time and length, to notice an edit.
fn config_stamp(path: &std::path::Path) -> Option<(Option<std::time::SystemTime>, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.modified().ok(), meta.len()))
}

/// Pick up a `"role"` edited in the config file while the dashboard runs.
///
/// Only the role is re-read, into [`AppState::role`]. The dashboard's own
/// `ConfigHandle` is left alone: it is also what the settings dialogs write
/// through, and swapping it under them is a larger change than this needs. The
/// daemon re-reads the same file on its own tick, so the board keys and the
/// notification feed change role together.
fn refresh_role(state: &mut AppState, stamp: &mut Option<(Option<std::time::SystemTime>, u64)>) {
    let now = config_stamp(state.config.path());
    if now == *stamp {
        return;
    }
    *stamp = now;
    let role = state.config.reloaded().role();
    if role != state.role {
        state.role = role;
        state.dirty = true;
    }
}

/// The Odoo client the worker uses, or nothing when the install is not
/// configured for Odoo — which is a supported way to run the dashboard.
fn odoo_client(config: &ConfigHandle) -> Option<std::sync::Arc<crate::odoo::OdooClient>> {
    let creds = config.odoo_creds();
    creds
        .is_complete()
        .then(|| std::sync::Arc::new(crate::odoo::OdooClient::new(creds)))
}

/// Which transport the dashboard runs on, decided exactly as the Node entry
/// point decided it: a daemon unless one is disabled, started if none is
/// answering and autostart is on, and an in-process engine if either of those
/// says no.
///
/// The second half of the answer is new, and is the whole point: when the
/// daemon could not be reached, the sentence saying why comes back with the
/// feed. Falling back was always right; falling back in silence is what made
/// a broken daemon and a quiet machine look identical.
fn open_feed(paths: &Paths, config: &ConfigHandle) -> (FeedHandle, Option<String>) {
    if !config.daemon_enabled() {
        return (FeedHandle::embedded(paths, group_paths(config)), None);
    }
    let port = protocol::resolve_port(config, paths, None);
    match client::ensure_daemon(paths, port, config.daemon_autostart()) {
        Ok(target) => (
            FeedHandle::remote(target.port, Box::new(RemoteFeed::connect(target.port))),
            None,
        ),
        Err(reason) => (
            FeedHandle::embedded(paths, group_paths(config)),
            Some(format!("{reason} Scanning from this dashboard instead.")),
        ),
    }
}

fn group_paths(config: &ConfigHandle) -> Vec<String> {
    config
        .groups()
        .iter()
        .map(|group| group.path.clone())
        .collect()
}

/// Whether Claude Code has ever written a transcript where this build looks
/// for them.
///
/// A first-ever run, a machine whose Claude home was relocated, an isolated
/// tree pointed at the wrong place: all three show an empty sessions pane, and
/// the pane can only say which if somebody asks the filesystem. Asked on an
/// empty tick and nowhere near a render path (brief §10 mandate #9).
fn refresh_transcripts_notice(state: &mut AppState) {
    let dir = state.paths.projects_dir.clone();
    let empty = std::fs::read_dir(&dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true);
    let notice = empty.then(|| {
        format!(
            "No transcripts under {} — start a session with `claude`, or set \
             CLAUDE_CONFIG_DIR if your Claude home is somewhere else.",
            dir.display()
        )
    });
    if state.transcripts_notice != notice {
        state.transcripts_notice = notice;
        state.dirty = true;
    }
}

pub(crate) fn open_conversation(
    state: &mut AppState,
    file: Option<PathBuf>,
) -> Option<TranscriptCursor> {
    let path = file?;
    let mut cursor = TranscriptCursor::open(&path, Collect::SessionAndConversation).ok()?;
    let _ = cursor.poll();
    state.conv.messages = cursor.messages().to_vec();
    state.conv.scroll_top = 0;
    state.conv.stick = true;
    sync_selected_meta(state);
    Some(cursor)
}

/// Refresh the conversation header and status footer from the session row,
/// which the scan keeps current even while the transcript is quiet.
fn sync_selected_meta(state: &mut AppState) {
    let Some(session_id) = state.selected_session_id.clone() else {
        return;
    };
    let meta = state
        .find_session(&session_id)
        .map(|session| ConversationMeta {
            last_usage: session.last_usage,
            last_timestamp: session.last_timestamp.clone().unwrap_or_default(),
            session_id: session.session_id.clone(),
            status: Some(session.status),
            activity_detail: session.activity_detail.clone(),
        });
    if meta.is_some() {
        state.conv.meta = meta;
        state.dirty = true;
    }
}
