//! A QA run: one stage's worth of tasks, reviewed together.
//!
//! The board answers "what is in QA". It does not answer the question a reviewer
//! actually has once several passes are in flight at once — which of them wants
//! me, and which are fine. That is what a run is for.
//!
//! Everything here is pure. It takes recorded state (what QAden wrote to disk,
//! what the scanner found running, which notifications arrived) and returns
//! rows. It starts nothing, reads no git and touches no clock of its own, so it
//! is safe on a render path and cheap to test.
//!
//! # Why a run is a record, not "the tasks in the QA stage"
//!
//! A stage is a fact about Odoo. A run is a decision someone made: these tasks,
//! reviewed together, starting now. They differ in ways that matter:
//!
//! * A task can leave the QA stage mid-run — it fails, and moves to a revision
//!   stage. It is still part of the run that found the problem.
//! * A task can arrive in the stage mid-run. It is not part of a run that
//!   started before it existed.
//! * "2 done of 7" is only meaningful against a denominator that does not move.
//!
//! So a run owns its task list from the moment it is created.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::qaden::{QaRunState, QaVerdict};
use crate::types::{Notification, Session};

/// A session silent for this long is called stalled. Matches the daemon's own
/// stall alert, so the board and the alert cannot disagree about one session.
pub const STALL: Duration = Duration::from_secs(15 * 60);

/// What one task in a run is doing.
///
/// The ordering of the variants is the escalation order, and [`QaStatus::rank`]
/// depends on it: `Asks` first because it is the only state actively costing the
/// reviewer time, `Stalled` next because a run that has gone quiet usually also
/// wants something and has failed to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QaStatus {
    Asks,
    Stalled,
    Revisions,
    Pass,
    Testing,
    Queued,
}

impl QaStatus {
    pub fn rank(self) -> u8 {
        self as u8
    }

    /// The glyph, which is all that survives on a narrow pane.
    pub fn glyph(self) -> &'static str {
        match self {
            QaStatus::Asks => "⏸",
            QaStatus::Stalled => "⚠",
            QaStatus::Revisions => "✗",
            QaStatus::Pass => "✓",
            QaStatus::Testing => "⠿",
            QaStatus::Queued => "○",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            QaStatus::Asks => "asks you",
            QaStatus::Stalled => "stalled",
            QaStatus::Revisions => "REVISION",
            QaStatus::Pass => "PASS",
            QaStatus::Testing => "testing",
            QaStatus::Queued => "queued",
        }
    }

    /// Whether this is a row the reviewer has to do something about.
    pub fn wants_attention(self) -> bool {
        matches!(self, QaStatus::Asks | QaStatus::Stalled)
    }
}

/// A run, as the dashboard holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaRun {
    pub id: String,
    pub project_name: String,
    pub stage_name: String,
    /// Fixed at creation. See the module docs for why this is not the stage.
    pub task_ids: Vec<i64>,
    pub started_at: String,
    /// `None` means "whatever the config says now", so raising the limit
    /// affects a run that is already open.
    pub lane_limit: Option<usize>,
    /// Tasks this run has started, so it cannot start one twice.
    pub spawned: Vec<i64>,
    pub mode: RunMode,
    /// Whether this run has launched a coordinator.
    ///
    /// Only whether, not which. WHICH session it is gets resolved live from
    /// `CLAUDE_SESSIONS_RUN_ID` every time it is needed — see
    /// [`coordinator_of`] — so a coordinator that dies stops being reported as
    /// present the moment its process goes, with nothing to invalidate.
    ///
    /// This flag exists only so the menu can refuse to start a second one
    /// during the seconds before the first appears in a scan.
    pub coordinator_started: bool,
}

/// The run id as it travels through a process environment.
///
/// Percent-encoded, because `ps -E` prints the whole environment as one
/// space-separated line and a value containing a space cannot be read back
/// from it — the reader stops at the space and gets half an id. Run ids are
/// `project::stage`, and stage names have spaces in them ("Quality
/// Assurance"), so this is the normal case rather than an edge one.
///
/// `%` is encoded first, or decoding an id that legitimately contains `%20`
/// would invent a space that was never there.
pub fn encode_run_id(run_id: &str) -> String {
    let mut out = String::with_capacity(run_id.len());
    for ch in run_id.chars() {
        match ch {
            '%' => out.push_str("%25"),
            c if c.is_whitespace() => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The inverse of [`encode_run_id`].
///
/// Surrounding quotes are stripped first: the launch writes `export KEY=...`
/// through a shell, and some platforms report the quoted form rather than the
/// value the shell ended up with.
pub fn decode_run_id(raw: &str) -> String {
    let trimmed = raw.trim_matches(|c| c == '\'' || c == '"');
    let bytes = trimmed.as_bytes();
    let mut out = String::with_capacity(trimmed.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 3 <= bytes.len() {
            if let Ok(code) = u8::from_str_radix(&trimmed[i + 1..i + 3], 16) {
                out.push(code as char);
                i += 3;
                continue;
            }
        }
        out.push(trimmed[i..].chars().next().unwrap_or_default());
        i += trimmed[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// Which session is the coordinator, given what is running now.
///
/// Matched on `CLAUDE_SESSIONS_RUN_ID`, which the launch exports and the
/// scanner reads back from `ps -E`. Nothing else carries it, so there is
/// nothing to disambiguate.
///
/// This replaced a guess: "a new session, in the launch folder, holding no
/// task". The run's own QA sessions satisfy all three for the moment between
/// writing a transcript and that transcript being read for a task URL, and they
/// start in the same folder seconds later — uncapped, seven of them at once. It
/// was a race, and losing it meant the dashboard calling a QA pass the
/// coordinator and typing every question into a session told not to answer
/// questions. The environment cannot be raced: the variable is set before the
/// process starts and is either there or not.
pub fn coordinator_of<'a>(
    run_id: &str,
    mut sessions: impl Iterator<Item = &'a Session>,
) -> Option<&'a Session> {
    if run_id.is_empty() {
        return None;
    }
    sessions.find(|session| session.run_id.as_deref() == Some(run_id))
}

/// How much a coordinating session is allowed to do on the reviewer's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// Record what it would have answered, answer nothing. A calibration mode:
    /// it measures the coordinator against the reviewer without letting it
    /// speak. Reachable from config, not from the run menu.
    Shadow,
    /// Answer questions of fact, and escalate everything else. The default,
    /// because a coordinator that answers nothing leaves every question with
    /// the reviewer, which is the job it was added to take on.
    ///
    /// Judgment calls, verdicts and Odoo writes still escalate or refuse — see
    /// the answer policy, which enforces that independently of the prompt.
    #[default]
    Triage,
}

impl QaRun {
    /// Run ids are stable per stage, so re-watching a stage finds its run
    /// rather than creating a second one over the same tasks.
    pub fn id_for(project_name: &str, stage_name: &str) -> String {
        format!("{project_name}::{stage_name}")
    }
}

/// One row of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEntry<'a> {
    pub task_id: i64,
    pub status: QaStatus,
    pub run: Option<&'a QaRunState>,
    pub session: Option<&'a Session>,
    pub ask: Option<&'a Notification>,
}

/// Everything a run needs to work out what its rows say.
#[derive(Debug, Default)]
pub struct RunCtx<'a> {
    pub run_states: HashMap<i64, QaRunState>,
    pub sessions: HashMap<i64, &'a Session>,
    /// The newest unanswered question per task.
    pub asks: HashMap<i64, &'a Notification>,
    /// Injected rather than read here, so every row in one frame is judged
    /// against the same instant and the module stays pure.
    pub now: Option<SystemTime>,
}

/// What a single task in a run is doing.
///
/// The order of these checks is the design, and two of them are counter-
/// intuitive enough to be worth stating:
///
/// A recorded verdict outranks a live session. `/qa` deliberately does not end
/// its session after parking a verdict — it prints the note and waits — so a
/// session still being alive says nothing about whether the work is done.
/// Ranking the session first would leave every finished pass reading "testing"
/// forever.
///
/// A stalled session outranks a recorded verdict, because a verdict left on
/// disk from an earlier round plus a session that died mid-round must not read
/// as finished.
///
/// There is no "this session finished" case, unlike the Node version. A session
/// that ends leaves the process scan entirely, so a session present here is by
/// definition still alive — the distinction Node needed does not exist.
pub fn qa_task_status(
    run: Option<&QaRunState>,
    session: Option<&Session>,
    asking: bool,
    now: Option<SystemTime>,
) -> QaStatus {
    // An outstanding question wins outright. It is the one state where the run
    // is stopped and the reviewer is the reason it is stopped.
    if asking {
        return QaStatus::Asks;
    }

    if let (Some(session), Some(now)) = (session, now) {
        // `duration_since` errs when the file is stamped in the future, which a
        // clock change can do. That is not silence, so it reads as zero.
        let silent = now
            .duration_since(session.session_mtime)
            .unwrap_or(Duration::ZERO);
        if silent >= STALL {
            return QaStatus::Stalled;
        }
    }

    match run.and_then(|r| r.verdict) {
        Some(QaVerdict::Pass) => return QaStatus::Pass,
        Some(QaVerdict::Revisions) => return QaStatus::Revisions,
        None => {}
    }

    if session.is_some() {
        return QaStatus::Testing;
    }
    if run.is_some_and(|r| r.exists && (r.open_gaps > 0 || r.closed_gaps > 0)) {
        return QaStatus::Testing;
    }
    QaStatus::Queued
}

/// A run's rows, sorted by what needs the reviewer most.
///
/// Ties break on task id so rows do not reshuffle under the cursor while two
/// share a status — the same reason the sessions tree holds a fixed order.
pub fn build_run_entries<'a>(run: &QaRun, ctx: &'a RunCtx<'a>) -> Vec<RunEntry<'a>> {
    let mut entries: Vec<RunEntry<'a>> = run
        .task_ids
        .iter()
        .map(|&task_id| {
            let state = ctx.run_states.get(&task_id);
            let session = ctx.sessions.get(&task_id).copied();
            let ask = ctx.asks.get(&task_id).copied();
            RunEntry {
                task_id,
                status: qa_task_status(state, session, ask.is_some(), ctx.now),
                run: state,
                session,
                ask,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        a.status
            .rank()
            .cmp(&b.status.rank())
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    entries
}

/// The counts a run header shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunSummary {
    pub total: usize,
    pub done: usize,
    pub asking: usize,
    pub stalled: usize,
    pub testing: usize,
    pub queued: usize,
}

impl RunSummary {
    /// Every task reached a verdict. An empty run is never finished — otherwise
    /// a run created a moment before its tasks load reads as complete.
    pub fn finished(&self) -> bool {
        self.total > 0 && self.done == self.total
    }

    /// Whether anything in this run is waiting on the reviewer.
    pub fn wants_attention(&self) -> bool {
        self.asking > 0 || self.stalled > 0
    }
}

pub fn run_summary(entries: &[RunEntry<'_>]) -> RunSummary {
    let count = |want: QaStatus| entries.iter().filter(|e| e.status == want).count();
    let done = count(QaStatus::Pass) + count(QaStatus::Revisions);
    RunSummary {
        total: entries.len(),
        done,
        asking: count(QaStatus::Asks),
        stalled: count(QaStatus::Stalled),
        testing: count(QaStatus::Testing),
        queued: count(QaStatus::Queued),
    }
}

/// One task somewhere that is waiting on the reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAsk {
    pub run_id: String,
    pub task_id: i64,
    pub status: QaStatus,
}

/// Everything across every run that wants the reviewer, in board order.
///
/// Deliberately flat across runs: with three runs open, "which one" is a
/// question the reviewer should not have to answer before reaching the next
/// thing that needs them. This is what the header counts and the jump key walks.
pub fn pending_asks(runs: &[QaRun], ctx: &RunCtx<'_>) -> Vec<PendingAsk> {
    let mut out = Vec::new();
    for run in runs {
        for entry in build_run_entries(run, ctx) {
            if entry.status.wants_attention() {
                out.push(PendingAsk {
                    run_id: run.id.clone(),
                    task_id: entry.task_id,
                    status: entry.status,
                });
            }
        }
    }
    out
}

/// The row key of the next thing wanting attention after `after`, wrapping.
///
/// Wrapping rather than stopping, so pressing the key repeatedly cycles instead
/// of sticking on the first one.
pub fn next_ask_key(runs: &[QaRun], ctx: &RunCtx<'_>, after: Option<&str>) -> Option<String> {
    let keys: Vec<String> = pending_asks(runs, ctx)
        .iter()
        .map(|ask| run_task_key(&ask.run_id, ask.task_id))
        .collect();
    if keys.is_empty() {
        return None;
    }
    let index = after
        .and_then(|key| keys.iter().position(|candidate| candidate == key))
        .map_or(0, |found| (found + 1) % keys.len());
    Some(keys[index].clone())
}

pub fn run_key(run_id: &str) -> String {
    format!("br:{run_id}")
}

pub fn run_task_key(run_id: &str, task_id: i64) -> String {
    format!("brt:{run_id}:{task_id}")
}

/// Whether a pane this wide can carry the full status column.
///
/// Measured rather than guessed. The ordinary board task row is already about
/// 70 plain characters at its longest, and the tree pane is 133 columns on a
/// 208-column terminal at the default conversation width, 50 on an 80-column
/// one. Below this, the full form wraps — and a wrapped row in a list pane is
/// worse than an abbreviated one.
pub const QA_WIDE_MIN_COLS: u16 = 90;

pub fn is_wide(tree_cols: u16) -> bool {
    tree_cols >= QA_WIDE_MIN_COLS
}

/// The status cell's text. `wide` adds the label and any detail; otherwise the
/// glyph carries the meaning alone.
pub fn qa_cell_text(entry: &RunEntry<'_>, wide: bool) -> String {
    if !wide {
        return entry.status.glyph().to_string();
    }
    let detail = match (entry.status, entry.run) {
        (QaStatus::Testing, Some(state)) if state.open_gaps + state.closed_gaps > 0 => {
            format!(
                " {}/{}",
                state.closed_gaps,
                state.open_gaps + state.closed_gaps
            )
        }
        // Which round an ask belongs to changes what the answer should be, so it
        // is worth the three characters once there has been more than one.
        (QaStatus::Asks, Some(state)) if state.round > 1 => format!(" r{}", state.round),
        _ => String::new(),
    };
    format!(
        "{} {}{}",
        entry.status.glyph(),
        entry.status.label(),
        detail
    )
}

/// The run header's counts. On a narrow pane only the two that change a
/// decision survive — how much is left, and how much is waiting — because the
/// rest is arithmetic the reader can do.
pub fn run_header_text(run: &QaRun, summary: &RunSummary, wide: bool) -> String {
    let mut parts = vec![format!(
        "{} task{}",
        summary.total,
        if summary.total == 1 { "" } else { "s" }
    )];
    if summary.done > 0 {
        parts.push(format!("{} done", summary.done));
    }
    if wide && summary.testing > 0 {
        parts.push(format!("{} testing", summary.testing));
    }
    if wide && summary.queued > 0 {
        parts.push(format!("{} queued", summary.queued));
    }
    if summary.asking > 0 {
        parts.push(format!("{} ask", summary.asking));
    }
    if summary.stalled > 0 {
        parts.push(format!("{} stalled", summary.stalled));
    }

    // The stage name is what the run is pinned under, so on a narrow pane it is
    // already on screen one row above.
    let title = if wide {
        format!("QA RUN · {}", run.stage_name)
    } else {
        "QA RUN".to_string()
    };
    format!("{title}  {}", parts.join(" · "))
}

mod answer;
mod schedule;
mod shadow;

pub use answer::{deliverable_session, may_answer, AnswerRefusal, MAX_ANSWER_CHARS};
pub use schedule::{admit, first_refusal, plan_spawns, AdmitCtx, Refusal};
pub use shadow::{Agreement, ShadowError, ShadowRecord, ShadowStore};

#[cfg(test)]
mod tests;
