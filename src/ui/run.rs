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
use crate::ui::actions::{ActionResult, ActionWorker};
use crate::ui::app::{body_area, draw};
use crate::ui::conversation::ConversationMeta;
use crate::ui::feed::{EmbeddedFeed, FeedEvent, SessionFeed};
use crate::ui::feed_remote::RemoteFeed;
use crate::ui::keys::handle_key;
use crate::ui::state::{Action, AppState, Quit};

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

    let worker = ActionWorker::start(driver_or_null(&config, policy), policy);
    let remote = connect_feed(&paths, &config);
    let mut state = AppState::new(paths.clone(), config);
    let mut feed: Box<dyn SessionFeed> = match remote {
        Some(remote) => Box::new(remote),
        None => Box::new(EmbeddedFeed::start(paths, group_paths(&state))),
    };

    let mut screen = CrosstermScreen::new();
    screen.acquire()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let result = event_loop(
        &mut terminal,
        &mut screen,
        &mut state,
        feed.as_mut(),
        &worker,
        policy,
    );

    screen.release()?;
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn event_loop(
    terminal: &mut Tui,
    screen: &mut CrosstermScreen,
    state: &mut AppState,
    feed: &mut dyn SessionFeed,
    worker: &ActionWorker,
    policy: SpawnPolicy,
) -> Result<()> {
    let mut conversation: Option<TranscriptCursor> = None;
    let mut last_clock = Instant::now();

    loop {
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

        for event in feed.drain() {
            match event {
                FeedEvent::Sessions {
                    by_project,
                    discovered,
                } => {
                    state.discovered_dirs = discovered;
                    state.apply_sessions(by_project);
                    sync_selected_meta(state);
                }
                FeedEvent::Notification(notification) => state.push_notification(*notification),
            }
        }

        for result in worker.drain() {
            match result {
                ActionResult::Flash(message) => state.flash(message),
                ActionResult::Refresh => feed.request_refresh(),
                ActionResult::Launched => {
                    feed.note_launch();
                    state.dirty = true;
                }
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

        for action in state.take_actions() {
            match action {
                Action::Refresh | Action::RefreshBurst => {
                    // The group list can have changed since the last scan (`a`
                    // and `d` edit it), and the scan thread owns its own copy —
                    // so a refresh re-syncs it. `set_groups` scans immediately.
                    feed.set_groups(group_paths(state));
                }
                Action::SelectSession { session_file, .. } => {
                    conversation = open_conversation(state, session_file);
                    state.dirty = true;
                }
                Action::LaunchSession { .. } => {
                    feed.note_launch();
                    worker.submit(action);
                }
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
                other => worker.submit(other),
            }
        }

        // The status bar carries a clock, so one redraw a second regardless.
        if last_clock.elapsed() >= Duration::from_secs(1) {
            last_clock = Instant::now();
            state.dirty = true;
        }

        if let Some(quit) = state.quit {
            // Plain `q` detaches and leaves the daemon working — that is the
            // point of it. Shift-Q stops the daemon too, so nothing is left
            // running in the background.
            if quit == Quit::ShutdownAll {
                feed.shutdown_daemon();
            }
            return Ok(());
        }
    }
}

/// Which transport the dashboard runs on, decided exactly as the Node entry
/// point decided it: a daemon unless one is disabled, started if none is
/// answering and autostart is on, and an in-process engine if either of those
/// says no. `None` here means "fall back to embedded".
fn connect_feed(paths: &Paths, config: &ConfigHandle) -> Option<RemoteFeed> {
    if !config.daemon_enabled() {
        return None;
    }
    let port = protocol::resolve_port(config, paths, None);
    let target = client::ensure_daemon(paths, port, config.daemon_autostart())?;
    Some(RemoteFeed::connect(target.port))
}

fn group_paths(state: &AppState) -> Vec<String> {
    state
        .config
        .groups()
        .iter()
        .map(|group| group.path.clone())
        .collect()
}

fn open_conversation(state: &mut AppState, file: Option<PathBuf>) -> Option<TranscriptCursor> {
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
