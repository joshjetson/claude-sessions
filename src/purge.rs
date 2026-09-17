//! Deciding which live sessions a purge may close.
//!
//! Ported from the Node app's `src/purge.js`. The point is to get finished work
//! off the tab bar without touching anything still live. Killing a task session
//! is not destructive on its own — the daemon archives the transcript of any
//! task session that disappears, so `v` reopens the whole conversation
//! afterwards — but killing an agent mid-task is, so the rule errs towards
//! keeping.
//!
//! It only purges stages it can NAME. Anything unrecognised is kept and
//! reported, rather than swept up by an "everything that isn't working" rule:
//! stage names differ per project ("QA" vs "Quality Assurance", "UAT" vs "User
//! Acceptance Testing"), and a stage nobody anticipated should show up and be
//! asked about, not silently killed.
//!
//! Everything here is pure: the dialog supplies the sessions and a stage
//! lookup, and gets back a plan. The killing lives in
//! [`crate::ui::actions`], behind the same [`crate::term::SpawnPolicy`] gate as
//! every other spawn.

use std::collections::BTreeMap;

use crate::types::Session;
use crate::util::word_match;

/// Which bucket a stage name falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Work is live, or is coming back to you. Never purged.
    Working,
    /// The agent is finished and someone is looking at it. Purged only when the
    /// caller opts in — the "I want to see what they did" bucket.
    Review,
    /// Finished and gone. Always purged.
    Done,
    /// A stage name nobody anticipated. Kept, and named in the dialog.
    Unknown,
}

/// Node wrote these as regexes; the anchored ones are [`Match::Exact`] and the
/// `\b…\b` ones are [`Match::Word`]. Alternations (`complete(d)?`,
/// `cancell?ed`, `needs? revision`) are spelled out as separate entries, which
/// is both clearer and cheaper than a matcher that has to understand them.
enum Match {
    Exact(&'static str),
    Word(&'static str),
}

impl Match {
    fn hits(&self, normalised: &str) -> bool {
        match self {
            Match::Exact(text) => normalised == *text,
            Match::Word(text) => word_match(normalised, text),
        }
    }
}

const WORKING: &[Match] = &[
    Match::Word("in progress"),
    Match::Word("in development"),
    Match::Exact("doing"),
    Match::Word("wip"),
    Match::Word("approved to start"),
    Match::Word("revision required"),
    Match::Word("need revision"),
    Match::Word("needs revision"),
];

const REVIEW: &[Match] = &[
    Match::Exact("qa"),
    Match::Word("quality assurance"),
    Match::Exact("uat"),
    Match::Word("user acceptance"),
    Match::Word("staging"),
    Match::Word("in review"),
    Match::Word("code review"),
    Match::Word("testing"),
];

const DONE: &[Match] = &[
    Match::Word("deployed"),
    Match::Exact("done"),
    Match::Word("complete"),
    Match::Word("completed"),
    Match::Word("closed"),
    Match::Word("merged"),
    Match::Word("cancelled"),
    Match::Word("canceled"),
    Match::Word("shipped"),
];

/// Lowercase, collapse whitespace runs, trim — Node's `norm`.
fn norm(stage: &str) -> String {
    stage
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Bucket a stage name. An empty name is [`Stage::Unknown`], never "finished".
pub fn classify_stage(stage: &str) -> Stage {
    let s = norm(stage);
    if s.is_empty() {
        return Stage::Unknown;
    }
    for (patterns, kind) in [
        (WORKING, Stage::Working),
        (REVIEW, Stage::Review),
        (DONE, Stage::Done),
    ] {
        if patterns.iter().any(|p| p.hits(&s)) {
            return kind;
        }
    }
    Stage::Unknown
}

/// What a purge needs to know about a session to close it: who to signal and
/// which tab to close. Deliberately not the whole [`Session`] — the plan is
/// carried into a worker thread and none of the transcript state travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeTarget {
    pub session_id: String,
    pub pids: Vec<u32>,
    pub tty: Option<String>,
    pub task_id: Option<i64>,
}

impl PurgeTarget {
    pub fn from_session(session: &Session) -> Self {
        PurgeTarget {
            session_id: session.session_id.clone(),
            pids: session.pids.clone(),
            tty: session.tty.clone(),
            task_id: session.task_id,
        }
    }
}

/// Why a session was left alone. Every kept session carries one, so the dialog
/// can say why rather than silently ignoring things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// Nobody can attribute it to a task — usually one you opened yourself. The
    /// dashboard's own session is one of these.
    NoTask,
    /// The board is filtered, or Odoo is unreachable. Absence of evidence.
    StageUnknown,
    StillWorking,
    UnderReview,
    UnrecognisedStage,
}

impl KeepReason {
    /// The phrase the dialog prints in parentheses. Matches Node's strings.
    pub fn label(self) -> &'static str {
        match self {
            KeepReason::NoTask => "no task",
            KeepReason::StageUnknown => "stage unknown",
            KeepReason::StillWorking => "still working",
            KeepReason::UnderReview => "under review",
            KeepReason::UnrecognisedStage => "unrecognised stage",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeEntry {
    pub target: PurgeTarget,
    pub stage: String,
    pub kind: Stage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptEntry {
    pub target: PurgeTarget,
    /// `None` when there is no stage to print — no task, or an unresolved one.
    pub stage: Option<String>,
    pub reason: KeepReason,
}

/// What a purge would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgePlan {
    pub purge: Vec<PurgeEntry>,
    pub keep: Vec<KeptEntry>,
}

impl PurgePlan {
    /// How many of each kind would be closed — what the dialog counts.
    pub fn counts(&self) -> PurgeCounts {
        PurgeCounts {
            purge: self.purge.len(),
            keep: self.keep.len(),
            review: self.kind_count(Stage::Review),
            done: self.kind_count(Stage::Done),
        }
    }

    fn kind_count(&self, kind: Stage) -> usize {
        self.purge.iter().filter(|e| e.kind == kind).count()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeCounts {
    pub purge: usize,
    pub keep: usize,
    pub review: usize,
    pub done: usize,
}

/// Work out what a purge would do.
///
/// `stage_of` answers "what stage is this task in", and `None` means "cannot
/// tell" — which keeps the session. Guessing "finished" there would kill
/// somebody else's live agent.
pub fn plan_purge(
    targets: &[PurgeTarget],
    stage_of: impl Fn(i64) -> Option<String>,
    include_review: bool,
) -> PurgePlan {
    let mut plan = PurgePlan::default();
    for target in targets {
        let Some(task_id) = target.task_id else {
            plan.keep.push(KeptEntry {
                target: target.clone(),
                stage: None,
                reason: KeepReason::NoTask,
            });
            continue;
        };
        let Some(stage) = stage_of(task_id) else {
            plan.keep.push(KeptEntry {
                target: target.clone(),
                stage: None,
                reason: KeepReason::StageUnknown,
            });
            continue;
        };
        let kind = classify_stage(&stage);
        if kind == Stage::Done || (kind == Stage::Review && include_review) {
            plan.purge.push(PurgeEntry {
                target: target.clone(),
                stage,
                kind,
            });
            continue;
        }
        plan.keep.push(KeptEntry {
            target: target.clone(),
            stage: Some(stage),
            reason: match kind {
                Stage::Working => KeepReason::StillWorking,
                Stage::Review => KeepReason::UnderReview,
                _ => KeepReason::UnrecognisedStage,
            },
        });
    }
    plan
}

/// One display group: a stage name and the entries in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageGroup<T> {
    pub stage: String,
    pub entries: Vec<T>,
}

/// Group a plan's entries by stage name for display, biggest group first.
///
/// One function for both halves of the plan — Node had the grouping inline and
/// the two callers passed different shapes. `label_of` is what makes it one
/// function: it supplies the group name for an entry that has no stage.
fn group_by<T: Clone>(entries: &[T], label_of: impl Fn(&T) -> String) -> Vec<StageGroup<T>> {
    // BTreeMap keeps ties in name order, so the same plan always groups the
    // same way — a HashMap would reshuffle equal-sized groups between frames.
    let mut groups: BTreeMap<String, Vec<T>> = BTreeMap::new();
    for entry in entries {
        groups
            .entry(label_of(entry))
            .or_default()
            .push(entry.clone());
    }
    let mut out: Vec<StageGroup<T>> = groups
        .into_iter()
        .map(|(stage, entries)| StageGroup { stage, entries })
        .collect();
    out.sort_by_key(|group| std::cmp::Reverse(group.entries.len()));
    out
}

pub fn group_purged(entries: &[PurgeEntry]) -> Vec<StageGroup<PurgeEntry>> {
    group_by(entries, |entry| entry.stage.clone())
}

pub fn group_kept(entries: &[KeptEntry]) -> Vec<StageGroup<KeptEntry>> {
    group_by(entries, |entry| match (&entry.stage, entry.reason) {
        (Some(stage), _) => stage.clone(),
        (None, KeepReason::NoTask) => "No task".to_string(),
        (None, _) => "Unknown".to_string(),
    })
}

#[cfg(test)]
mod tests;
