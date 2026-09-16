//! Stage names, task states, and the one function that resolves a stage.
//!
//! Resolution is name-based, never positional: stage ids differ per project and
//! the column order is whatever the project manager last dragged it into. The
//! Node app had `resolveDoneStage` and `resolveInProgressStage` with identical
//! bodies and different constant lists — here that is [`resolve_stage`] taking
//! the list as a parameter ([`StageKind`]).

/// QA-stage names seen across projects, in priority order. A finished task goes
/// to QA — never to a "Revision Required" stage, which is for QA-found bugs.
pub const DEFAULT_DONE_STAGES: &[&str] = &[
    "Quality Assurance",
    "QA",
    "Ready for QA",
    "QA Review",
    "Testing",
    "UAT",
    "User Acceptance Testing",
    // Fallback for projects with no QA stage: review before deploy.
    "Staging",
];

/// Working-stage names, in priority order. A task moves here when work starts
/// (or when a revision is picked up) so the board shows it is being worked on.
pub const DEFAULT_IN_PROGRESS_STAGES: &[&str] = &[
    "In Progress",
    "In-Progress",
    "Doing",
    "In Development",
    "Development",
    "Working",
    "WIP",
    "Started",
];

/// `project.task.state` values. A separate axis from the stage/column: a task
/// can sit in "Deployed" while its state is still "In Progress".
pub mod task_state {
    pub const IN_PROGRESS: &str = "01_in_progress";
    pub const CHANGES_REQUESTED: &str = "02_changes_requested";
    /// The "Complete" state — NOT the "Done" checkmark.
    pub const COMPLETE: &str = "03_approved";
    pub const DONE: &str = "1_done";
    pub const CANCELLED: &str = "1_canceled";
    pub const WAITING: &str = "04_waiting_normal";
}

/// "Closed" in Odoo's dependency sense: a blocker in one of these states has
/// landed and no longer holds anything up.
pub const CLOSED_STATES: &[&str] = &[
    task_state::DONE,
    task_state::CANCELLED,
    task_state::COMPLETE,
];

/// A stage as Odoo stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRecord {
    pub id: i64,
    pub name: String,
    pub sequence: i64,
}

/// Where a task is being moved to. The variant carries the default name list
/// and the fuzzy fallback, so callers never repeat either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    /// The QA-ish stage a finished task moves into.
    Done,
    /// The working stage a started task moves into.
    InProgress,
}

impl StageKind {
    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            StageKind::Done => DEFAULT_DONE_STAGES,
            StageKind::InProgress => DEFAULT_IN_PROGRESS_STAGES,
        }
    }

    /// The last-resort match, kept as code rather than a regex crate: Node used
    /// `/quality assurance|\bqa\b/i` and `/in\s*progress|\bdoing\b|\bwip\b/i`.
    fn fuzzy(self, name: &str) -> bool {
        let lower = name.to_lowercase();
        match self {
            StageKind::Done => lower.contains("quality assurance") || contains_word(&lower, "qa"),
            StageKind::InProgress => {
                // `in\s*progress` — only whitespace may sit between the words,
                // so comparing with whitespace removed is the same test.
                strip_whitespace(&lower).contains("inprogress")
                    || contains_word(&lower, "doing")
                    || contains_word(&lower, "wip")
            }
        }
    }
}

/// The resolved stage to move a task into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageMatch {
    pub id: i64,
    pub name: String,
}

/// Resolve the stage a task should move into, or `None` to leave it alone.
///
/// Order: the configured name(s), then the known names for this [`StageKind`],
/// then a fuzzy match. `None` is a real answer — the task stays where it is.
/// Guessing "the next column" is what would land a finished task in "Revision
/// Required", so it is never attempted.
pub fn resolve_stage(
    stages: &[StageRecord],
    kind: StageKind,
    preferred: &[String],
) -> Option<StageMatch> {
    if stages.is_empty() {
        return None;
    }

    let wanted = preferred
        .iter()
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .chain(kind.defaults().iter().copied());

    for name in wanted {
        if let Some(stage) = stages
            .iter()
            .find(|stage| stage.name.eq_ignore_ascii_case(name))
        {
            return Some(StageMatch {
                id: stage.id,
                name: stage.name.clone(),
            });
        }
    }

    stages
        .iter()
        .find(|stage| kind.fuzzy(&stage.name))
        .map(|stage| StageMatch {
            id: stage.id,
            name: stage.name.clone(),
        })
}

/// `\bword\b` against an already-lowercased haystack: the match must not be
/// flanked by other word characters, so "QA" hits "QA Review" but not "Aqua".
fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(word) {
        let start = from + offset;
        let end = start + word.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn strip_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
