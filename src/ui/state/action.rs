//! The work the draw thread refuses to do itself.
//!
//! Every key handler is a pure function of `(state, key)`; anything that would
//! block — spawning a terminal, writing a prompt file, asking Odoo for a stage
//! list — leaves as one of these and is run by [`crate::ui::actions`] on its own
//! thread (brief §10 mandate #9). That is also what makes a whole key sequence
//! replayable in a test: the queue is the record of what *would* have happened.

use std::path::PathBuf;

use crate::odoo::FetchBoardOptions;
use crate::term::SessionRef;
use crate::types::{NotificationLevel, NotificationStatus};
use crate::ui::board::{LaunchSpec, ResumeRequest, SendSpec};

/// Work the UI thread refuses to do itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask the feed for a fresh scan now.
    Refresh,
    /// Refresh at 0/400/1000ms. SIGTERM is not instant — the process lingers in
    /// `ps` for a moment — so Node polled three times after a kill, and a single
    /// refresh here would redraw the row it just killed.
    RefreshBurst,
    /// Read this transcript into the conversation pane.
    SelectSession {
        session_id: String,
        session_file: Option<PathBuf>,
    },
    FocusTerminal(Box<SessionRef>),
    LaunchSession {
        cwd: String,
    },
    Kill {
        pids: Vec<u32>,
        label: String,
    },
    /// Hand the terminal to `$EDITOR` and take it back — Node's `suspendUI`.
    OpenEditor {
        path: String,
        line: u32,
    },
    /// Take a plan-usage reading. Only reached when no daemon owns the check.
    RefreshUsage,
    /// Hand a path to the desktop's default handler — the daily log's `o`.
    OpenPath(String),
    /// Close finished sessions: SIGTERM the agent, then close its tab.
    Purge(Box<Vec<crate::purge::PurgeEntry>>),
    /// Fill the QAden head cache for a task, then re-label the open menu.
    RefreshQaState {
        task_id: i64,
    },

    // --- board ---------------------------------------------------------------
    /// Fetch the Odoo board. The options are built where the config and the
    /// filter live, so the worker holds no view state of its own.
    RefreshBoard(Box<FetchBoardOptions>),
    /// Load what an open dialog is waiting on. One variant per lookup rather
    /// than a generic query, so an unknown answer cannot reach the wrong
    /// dialog.
    FetchBlockers {
        task_id: i64,
        blocker_ids: Vec<i64>,
    },
    FetchStages {
        task_id: i64,
        project_id: i64,
    },
    FetchProjects,
    FetchTaskDescription {
        task_id: i64,
    },
    /// Stage names for the sessions a purge is about to judge.
    FetchTaskStages {
        task_ids: Vec<i64>,
    },
    /// Start, revise or resume — everything already resolved.
    Launch(Box<LaunchSpec>),
    /// Type a revision into a session that is already open, rather than
    /// resuming its id into a second process against the same conversation.
    SendToSession(Box<SendSpec>),
    /// Find a task's archived conversation and open it. The lookup restores the
    /// transcript into Claude Code's own project directory, so it is file I/O
    /// and belongs off the draw thread.
    Resume(Box<ResumeRequest>),
    /// The explicit move from the stage picker: the user named the stage, so
    /// nothing is resolved and nothing is guessed.
    MoveStage {
        task_id: i64,
        stage_id: i64,
        stage_name: String,
    },
    /// `open <url>` — the task form in a browser.
    OpenUrl(String),
    /// A terminal on the server a project runs on.
    Ssh {
        project: String,
    },
    Sound(NotificationLevel),
    /// Write a repo's starter `pipeline.json`, from the pipeline viewer's `t`.
    WritePipelineTemplate {
        repo: String,
        pipeline_id: String,
    },
    /// A notification changed. `None` means dismissed.
    Notifications {
        ids: Vec<String>,
        status: Option<NotificationStatus>,
    },
}
