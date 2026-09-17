//! The two markdown parsers: reasoning-journal entries and project rules.
//!
//! These are the join between the journals and the task archive — when they
//! silently fail the viewer just shows fewer entries, with no error anywhere,
//! which is why they are the most heavily tested part of this module.

use std::fs;
use std::path::Path;

use serde::Serialize;

use super::classify::{
    classify, extract_task_ids, mentions_revision, prose_assumption, prose_correction, SectionKind,
};
use super::markdown::{
    has_line_prefix, markdown_bullet, markdown_heading, split_at_prefix, split_first_line,
    split_keeping_preamble, split_paragraphs,
};
use super::{base_name, journal_path, rules_path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSection {
    pub label: String,
    pub body: String,
    pub kind: SectionKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    /// `<repo>:<index>` — stable while the file's entry order is.
    pub id: String,
    pub repo: String,
    pub repo_path: String,
    pub file: String,
    pub date: Option<String>,
    pub title: String,
    pub task_ids: Vec<i64>,
    pub sections: Vec<JournalSection>,
    pub body: String,
    pub words: usize,
    pub is_revision: bool,
    pub has_assumption: bool,
    pub has_correction: bool,
    pub has_attempts: bool,
    pub has_rule: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub repo: String,
    pub repo_path: String,
    pub file: String,
    pub section: String,
    pub text: String,
    pub task_ids: Vec<i64>,
}

/// Parse a repo's reasoning journal. A missing or empty file is no entries,
/// never an error.
pub fn parse_journal(repo_path: &Path) -> Vec<JournalEntry> {
    let file = journal_path(repo_path);
    let Ok(md) = fs::read_to_string(&file) else {
        return Vec::new();
    };
    let repo = base_name(repo_path);
    // Drop the "# Problem Reasoning Journal" preamble: everything before the
    // first `## ` heading.
    split_at_prefix(&md, "##")
        .into_iter()
        .enumerate()
        .map(|(index, block)| {
            let (heading_line, body) = split_first_line(block);
            let body = body.trim();
            let (date, title) = parse_heading(heading_line);
            let sections = parse_sections(body);
            let kinds: Vec<SectionKind> = sections.iter().map(|s| s.kind).collect();
            let lower_body = body.to_lowercase();
            JournalEntry {
                id: format!("{repo}:{index}"),
                task_ids: extract_task_ids(heading_line),
                is_revision: mentions_revision(&title),
                has_assumption: kinds.contains(&SectionKind::Assumption)
                    || prose_assumption(&lower_body),
                // Compact entries rarely carry a "User Correction" heading — the
                // correction shows up as prose ("the user pointed me to…", "QA
                // correctly flagged…").
                has_correction: kinds.contains(&SectionKind::Correction)
                    || prose_correction(&lower_body),
                has_attempts: kinds.contains(&SectionKind::Attempts),
                has_rule: kinds.contains(&SectionKind::Rule)
                    || lower_body.contains("reusable rule"),
                words: body.split_whitespace().count(),
                repo: repo.clone(),
                repo_path: repo_path.display().to_string(),
                file: file.display().to_string(),
                date,
                title,
                sections,
                body: body.to_string(),
            }
        })
        .collect()
}

/// Parse a repo's `project-rules.md`: bullets, the heading they sit under, and
/// continuation lines folded into the bullet above them.
pub fn parse_rules(repo_path: &Path) -> Vec<Rule> {
    let file = rules_path(repo_path);
    let Ok(md) = fs::read_to_string(&file) else {
        return Vec::new();
    };
    let repo = base_name(repo_path);
    let mut rules: Vec<Rule> = Vec::new();
    let mut section = String::new();
    let mut current: Option<Rule> = None;

    // A rule is only kept once it has text: a bullet marker with nothing after
    // it is punctuation, not a rule.
    fn flush(rules: &mut Vec<Rule>, current: &mut Option<Rule>) {
        if let Some(rule) = current.take() {
            if !rule.text.trim().is_empty() {
                rules.push(rule);
            }
        }
    }

    for line in md.split('\n') {
        if let Some(heading) = markdown_heading(line) {
            flush(&mut rules, &mut current);
            section = heading.to_string();
            continue;
        }
        if let Some(bullet) = markdown_bullet(line) {
            flush(&mut rules, &mut current);
            current = Some(Rule {
                id: format!("{repo}:{}", rules.len()),
                repo: repo.clone(),
                repo_path: repo_path.display().to_string(),
                file: file.display().to_string(),
                section: section.clone(),
                text: bullet.to_string(),
                task_ids: Vec::new(),
            });
            continue;
        }
        match (&mut current, line.trim().is_empty()) {
            (Some(rule), false) => {
                rule.text.push('\n');
                rule.text.push_str(line.trim());
            }
            (_, true) => flush(&mut rules, &mut current),
            _ => {}
        }
    }
    flush(&mut rules, &mut current);

    for rule in &mut rules {
        rule.task_ids = extract_task_ids(&rule.text);
    }
    rules
}

// --- heading and ids --------------------------------------------------------

/// `[2026-07-23] Task-5944 REVISION — short problem title`, or a bare title.
fn parse_heading(line: &str) -> (Option<String>, String) {
    let rest = line.trim_start();
    let rest = rest.strip_prefix('[').unwrap_or(rest);
    if let Some((date, after)) = take_iso_date(rest) {
        let after = after.strip_prefix(']').unwrap_or(after);
        let after = after.trim_start();
        // An optional leading dash, which the title itself should not keep.
        let after = after
            .strip_prefix(['—', '–', '-'])
            .map(str::trim_start)
            .unwrap_or(after);
        let title = after.trim();
        return (
            Some(date.to_string()),
            if title.is_empty() {
                "(untitled)".to_string()
            } else {
                title.to_string()
            },
        );
    }
    let title = line.trim();
    (
        None,
        if title.is_empty() {
            "(untitled)".to_string()
        } else {
            title.to_string()
        },
    )
}

/// A leading `YYYY-MM-DD`, and what follows it.
fn take_iso_date(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let shaped = bytes[..10].iter().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 {
            *b == b'-'
        } else {
            b.is_ascii_digit()
        }
    });
    shaped.then(|| s.split_at(10))
}

// --- sections ---------------------------------------------------------------

/// Entries use either the full template (`###` sections) or the compact one
/// (`**Bold label:**` paragraphs). Both normalise to the same shape.
fn parse_sections(body: &str) -> Vec<JournalSection> {
    if has_line_prefix(body, "###") {
        return parse_heading_sections(body);
    }
    parse_compact_sections(body)
}

fn parse_heading_sections(body: &str) -> Vec<JournalSection> {
    let mut out = Vec::new();
    let mut parts = split_keeping_preamble(body, "###");
    let preamble = parts.remove(0);
    if !preamble.trim().is_empty() {
        out.push(section("", preamble.trim()));
    }
    for part in parts {
        let (label, rest) = split_first_line(part);
        out.push(section(label.trim(), rest.trim()));
    }
    out
}

fn parse_compact_sections(body: &str) -> Vec<JournalSection> {
    let mut out: Vec<JournalSection> = Vec::new();
    for paragraph in split_paragraphs(body) {
        let text = paragraph.trim();
        if text.is_empty() {
            continue;
        }
        match bold_label(text) {
            Some((label, rest)) => out.push(section(&label, rest.trim())),
            // Prose after a labelled paragraph belongs to it; prose before any
            // label is an unlabelled opening section.
            None => match out.last_mut() {
                Some(last) => {
                    last.body.push_str("\n\n");
                    last.body.push_str(text);
                }
                None => out.push(section("", text)),
            },
        }
    }
    out
}

fn section(label: &str, body: &str) -> JournalSection {
    JournalSection {
        kind: classify(label),
        label: label.to_string(),
        body: body.to_string(),
    }
}

/// `**Root cause:**  body` → `("Root cause", "body")`.
fn bold_label(text: &str) -> Option<(String, &str)> {
    let rest = text.strip_prefix("**")?;
    let close = rest.find("**")?;
    let inner = rest[..close].trim();
    // The label is one line; a `**` that closes several lines later is emphasis
    // inside a paragraph, not a heading.
    if inner.is_empty() || inner.contains('\n') {
        return None;
    }
    let label = inner.trim_end_matches([':', '.']).trim().to_string();
    if label.is_empty() {
        return None;
    }
    let after = rest[close + 2..].trim_start();
    let after = after
        .strip_prefix(':')
        .map(str::trim_start)
        .unwrap_or(after);
    Some((label, after))
}
