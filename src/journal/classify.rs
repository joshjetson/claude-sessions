//! What a journal section is about, and the flags an entry is filtered by.
//!
//! Nine kinds, matched against a section's heading in a fixed order — Node held
//! these as a list of regexes and took the first hit, and the order is
//! load-bearing ("Final Fix" must read as a fix, not as a rule). The derived
//! flags also look at the body: compact entries rarely carry a "User
//! Correction" heading, so a correction usually shows up as prose.

use serde::Serialize;

use crate::util::{any_word_match, word_match};

/// What a section is about, from its heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SectionKind {
    Rule,
    RootCause,
    Fix,
    Assumption,
    Correction,
    Attempts,
    Investigation,
    Validation,
    Problem,
    Other,
}

/// Which of the nine kinds a heading names. First match wins, in this order.
pub(super) fn classify(label: &str) -> SectionKind {
    let l = label.to_lowercase();
    let l = l.trim();
    if l.contains("reusable rule")
        || l.contains("lesson learned")
        || l.contains("lesson")
        || l.ends_with("rule")
        || l.ends_with("rules")
    {
        return SectionKind::Rule;
    }
    if l.contains("root cause") {
        return SectionKind::RootCause;
    }
    if l.starts_with("fix")
        || l.starts_with("final fix")
        || l.contains("what fixed")
        || l.contains("resolution")
    {
        return SectionKind::Fix;
    }
    if l.contains("initial assumption")
        || l.contains("initial understanding")
        || l.contains("wrong assumption")
        || l.contains("first thought")
        || l.contains("assumed")
    {
        return SectionKind::Assumption;
    }
    if l.contains("user correction")
        || l.contains("user guidance")
        || l.contains("correction or guidance")
        || l.contains("user pointed")
    {
        return SectionKind::Correction;
    }
    if l.contains("attempts")
        || l.contains("did not work")
        || l.contains("did not fully work")
        || l.contains("failed")
    {
        return SectionKind::Attempts;
    }
    if l.contains("investigation")
        || l.contains("investigated")
        || l.contains("what was actually")
        || l.contains("second finding")
        || l.contains("concealed")
    {
        return SectionKind::Investigation;
    }
    if l.contains("validation") || l.contains("validated") || l.contains("how it was tested") {
        return SectionKind::Validation;
    }
    if l.contains("problem") {
        return SectionKind::Problem;
    }
    SectionKind::Other
}

// --- derived flags ----------------------------------------------------------

/// `\brev(ision|\.|\b)` — "REVISION" and "rev." count, "reverted" does not.
pub(super) fn mentions_revision(title: &str) -> bool {
    let lower = title.to_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("rev") {
        let start = from + offset;
        from = start + 3;
        if start > 0 && is_word_byte(bytes[start - 1]) {
            continue;
        }
        let after = &lower[start + 3..];
        if after.starts_with("ision")
            || after.starts_with('.')
            || after
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        {
            return true;
        }
    }
    false
}

pub(super) fn prose_assumption(lower_body: &str) -> bool {
    lower_body.contains("initial assumption")
        || any_word_match(
            lower_body,
            &[
                "i thought",
                "i assumed",
                "i first thought",
                "i first assumed",
            ],
        )
}

/// Corrections written as prose rather than under a heading.
pub(super) fn prose_correction(lower_body: &str) -> bool {
    const USER_VERBS: [&str; 6] = ["pointed", "corrected", "flagged", "caught", "said", "noted"];
    const QA_VERBS: [&str; 4] = ["found", "flagged", "caught", "noted"];
    if USER_VERBS
        .iter()
        .any(|verb| word_match(lower_body, &format!("user {verb}")))
    {
        return true;
    }
    QA_VERBS.iter().any(|verb| {
        word_match(lower_body, &format!("qa {verb}"))
            || word_match(lower_body, &format!("qa correctly {verb}"))
    })
}

// --- task ids ---------------------------------------------------------------

/// Task ids as journal titles and rules write them: `Task-5944`, `Task 6117`,
/// `task-5699`, `#5882`. Order is first-seen, which is what the viewer shows.
pub fn extract_task_ids(text: &str) -> Vec<i64> {
    let lower = text.to_lowercase();
    let bytes = lower.as_bytes();
    let mut ids: Vec<i64> = Vec::new();
    let push = |id: i64, ids: &mut Vec<i64>| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };

    // `\btask[-\s#]*(\d{3,6})\b`
    let mut from = 0;
    while let Some(offset) = lower[from..].find("task") {
        let start = from + offset;
        from = start + 4;
        let boundary = start == 0 || !is_word_byte(bytes[start - 1]);
        if !boundary {
            continue;
        }
        let mut cursor = from;
        while cursor < bytes.len() && matches!(bytes[cursor], b'-' | b' ' | b'\t' | b'\n' | b'#') {
            cursor += 1;
        }
        if let Some(id) = digits_at(bytes, cursor, 3, 6) {
            push(id, &mut ids);
        }
    }

    // `#(\d{4,6})\b`
    let mut from = 0;
    while let Some(offset) = lower[from..].find('#') {
        let start = from + offset;
        from = start + 1;
        if let Some(id) = digits_at(bytes, from, 4, 6) {
            push(id, &mut ids);
        }
    }
    ids
}

/// A run of `min..=max` digits at `at`, ending on a word boundary.
fn digits_at(bytes: &[u8], at: usize, min: usize, max: usize) -> Option<i64> {
    let mut end = at;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    let len = end - at;
    if len < min || len > max {
        return None;
    }
    if end < bytes.len() && is_word_byte(bytes[end]) {
        return None;
    }
    std::str::from_utf8(&bytes[at..end]).ok()?.parse().ok()
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
