//! The standup log: one markdown line per completed task per day.
//!
//! Ported from the Node app's `src/dailylog.js`. Each finished task appends
//! `logs/YYYY-MM-DD.md` with
//! `- **#<id> <title>** — <short> ([MR](url))  _(hh:mm AM)_`
//! and the same entry as a row in SQLite. The markdown is the human artifact
//! and what the `l` viewer renders; the row is what makes an entry answerable
//! later — per-task history across days is a question the per-day files cannot
//! answer at all.
//!
//! Reading a line back is [`crate::db::parse_log_line`], not a second parser
//! here: Node had three copies of that regex (the importer, the journal join,
//! the log dialog) and they had already drifted.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::db::{log_day_from_filename, parse_log_line, DailyLogEntry, Db, LogLine};
use crate::paths::Paths;

/// The placeholder [`ensure_daily_log`] writes, and the marker
/// [`append_daily_log`] replaces rather than appends to.
const PLACEHOLDER: &str = "_No completed tasks logged yet._";

/// Cap on a standup line, and the ellipsis budget inside it.
const SHORT_CAP: usize = 180;

/// `YYYY-MM-DD` for a local timestamp — the day a log file is named after.
pub fn ymd(at: DateTime<Local>) -> String {
    at.format("%Y-%m-%d").to_string()
}

pub fn daily_log_path(paths: &Paths, day: &str) -> PathBuf {
    paths.logs_dir.join(format!("{day}.md"))
}

/// Every day that has a log, oldest first.
///
/// The union of both stores: the markdown is what `l` renders, and the database
/// may know about days whose file was moved or deleted.
pub fn list_log_dates(paths: &Paths, db: Option<&Db>) -> Vec<String> {
    let mut days: BTreeSet<String> = db
        .map(|db| db.list_log_days().into_iter().collect())
        .unwrap_or_default();
    if let Ok(entries) = fs::read_dir(&paths.logs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(day) = log_day_from_filename(&name) {
                days.insert(day.to_string());
            }
        }
    }
    days.into_iter().collect()
}

/// A day's markdown, or empty when there is none.
pub fn read_log_for(paths: &Paths, day: &str) -> String {
    fs::read_to_string(daily_log_path(paths, day)).unwrap_or_default()
}

/// One parsed entry, with the day it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayEntry {
    pub day: String,
    pub line: LogLine,
}

/// Parse a day's markdown into entries, skipping the heading and anything that
/// is not an entry line.
pub fn parse_day(day: &str, raw: &str) -> Vec<DayEntry> {
    raw.lines()
        .filter_map(parse_log_line)
        .map(|line| DayEntry {
            day: day.to_string(),
            line,
        })
        .collect()
}

/// Every entry in every day's file, oldest day first.
pub fn read_all_entries(paths: &Paths, db: Option<&Db>) -> Vec<DayEntry> {
    list_log_dates(paths, db)
        .into_iter()
        .flat_map(|day| {
            let raw = read_log_for(paths, &day);
            parse_day(&day, &raw)
        })
        .collect()
}

/// The `# Daily log — <day>` heading a file opens with.
fn heading(day: &str) -> String {
    format!("# Daily log — {day}\n\n")
}

/// Make sure a day's log exists so it can be opened, returning its path.
pub fn ensure_daily_log(paths: &Paths, day: &str) -> PathBuf {
    let path = daily_log_path(paths, day);
    if !path.exists() {
        let _ = fs::create_dir_all(&paths.logs_dir);
        let _ = fs::write(&path, format!("{}{PLACEHOLDER}\n", heading(day)));
    }
    path
}

/// What one completed task contributes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogRequest {
    pub task_id: i64,
    pub title: String,
    /// The agent's full sign-off text; [`short_summary`] condenses it.
    pub summary: String,
    pub mr_url: Option<String>,
}

/// Append a task's line to today's log and write the matching row.
///
/// Returns the file written, or `None` for a request with no task id — the
/// same refusal Node's `if (!taskId) return null` made.
pub fn append_daily_log(
    paths: &Paths,
    db: Option<&Db>,
    request: &LogRequest,
    at: DateTime<Local>,
) -> Option<PathBuf> {
    if request.task_id == 0 {
        return None;
    }
    let day = ymd(at);
    let path = daily_log_path(paths, &day);
    let _ = fs::create_dir_all(&paths.logs_dir);

    let short = {
        let condensed = short_summary(&request.summary);
        if condensed.is_empty() {
            "Completed.".to_string()
        } else {
            condensed
        }
    };
    let mr = request
        .mr_url
        .as_deref()
        .filter(|url| !url.is_empty())
        .map(|url| format!(" ([MR]({url}))"))
        .unwrap_or_default();
    let line = format!(
        "- **#{} {}** — {short}{mr}  _({})_\n",
        request.task_id,
        request.title.trim(),
        at.format("%I:%M %p")
    );

    let existing = fs::read_to_string(&path).unwrap_or_default();
    // A fresh placeholder file is replaced, not appended to.
    let mut body = if existing.is_empty() || existing.contains(PLACEHOLDER) {
        heading(&day)
    } else {
        existing
    };
    body.push_str(&line);
    let _ = fs::write(&path, body);

    if let Some(db) = db {
        db.put_daily_log_entry(&DailyLogEntry {
            day,
            ts: at
                .to_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            task_id: Some(request.task_id),
            title: request.title.trim().to_string(),
            summary: request.summary.clone(),
            short,
            mr_url: request.mr_url.clone().filter(|url| !url.is_empty()),
        });
    }
    Some(path)
}

/// Open the file for a day, creating it if needed — what `o` in the log viewer
/// hands to `open`.
pub fn openable_log(paths: &Paths, day: &str) -> PathBuf {
    ensure_daily_log(paths, day)
}

// --- condensing -------------------------------------------------------------

/// Condense the agent's Root-cause / Fix / Before-after write-up into one
/// standup line.
///
/// Prefers the `**Fix:**` section — what was actually done — and falls back to
/// the whole thing. Then strips markdown, keeps the first sentence when it is
/// substantial, and hard-caps the rest.
pub fn short_summary(summary: &str) -> String {
    if summary.trim().is_empty() {
        return String::new();
    }
    let body = fix_section(summary).unwrap_or(summary);
    let text = strip_markdown(body);
    let text = first_sentence(&text).unwrap_or(text);
    cap(&text)
}

/// The body of a `**Fix:**` / `**fix**` / `**FIX:**` section, up to the next
/// blank line or bold heading.
///
/// Node's `/\*\*\s*fix\s*:?\s*\*\*\s*([\s\S]*?)(?:\n\s*\n|\n\s*\*\*|$)/i`.
fn fix_section(summary: &str) -> Option<&str> {
    let lower = summary.to_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("**") {
        let open = from + offset;
        from = open + 2;
        let Some(start) = fix_heading_end(&lower[open..]).map(|end| open + end) else {
            continue;
        };
        let body = &summary[start..];
        let body = body.trim_start_matches([' ', '\t', '\n', '\r']);
        return Some(&body[..section_end(body)]);
    }
    None
}

/// Length of a `**fix:**` heading at the start of `lower`, or `None`.
fn fix_heading_end(lower: &str) -> Option<usize> {
    let rest = lower.strip_prefix("**")?;
    let mut used = 2;
    let trimmed = rest.trim_start_matches([' ', '\t']);
    used += rest.len() - trimmed.len();
    let rest = trimmed.strip_prefix("fix")?;
    used += 3;
    let trimmed = rest.trim_start_matches([' ', '\t']);
    used += rest.len() - trimmed.len();
    let (rest, used) = match trimmed.strip_prefix(':') {
        Some(after) => (after, used + 1),
        None => (trimmed, used),
    };
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let used = used + (rest.len() - trimmed.len());
    trimmed.strip_prefix("**").map(|_| used + 2)
}

/// Where a section body ends: the first blank line, the first bold heading on a
/// line of its own, or the end of the text.
fn section_end(body: &str) -> usize {
    let bytes = body.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let mut after = index + 1;
        let mut saw_newline = false;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            saw_newline |= bytes[after] == b'\n';
            after += 1;
        }
        // `\n\s*\n` — a blank line closes the section.
        if saw_newline {
            return index;
        }
        // `\n\s*\*\*` — the next bold heading closes it.
        if body[after..].starts_with("**") {
            return index;
        }
    }
    body.len()
}

/// Drop bold headers, markdown punctuation and bullet markers, then collapse
/// whitespace. Node did this as four chained replaces; the order is the same.
fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // `\*\*[^*]*\*\*` → " ". Bold *headers* in the fallback path, which are
    // labels rather than content.
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let close = after.find("**");
        let plain_run = close.filter(|end| !after[..*end].contains('*'));
        match plain_run {
            Some(end) => {
                out.push_str(&rest[..open]);
                out.push(' ');
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[..open + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);

    // `[*`#>]` → " ", then bullet markers at the start of each line.
    let cleaned: String = out
        .chars()
        .map(|ch| match ch {
            '*' | '`' | '#' | '>' => ' ',
            other => other,
        })
        .collect();
    let debulleted: Vec<String> = cleaned
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            match trimmed.strip_prefix(['-', '•']) {
                Some(rest) => rest.trim_start_matches([' ', '\t']).to_string(),
                None => line.to_string(),
            }
        })
        .collect();
    debulleted
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first sentence, when it carries enough to stand alone.
///
/// Node's threshold: 25 characters. "Done." is not a standup line; "Rewrote the
/// retry loop to use exponential backoff." is.
fn first_sentence(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if !matches!(byte, b'.' | b'!' | b'?') {
            continue;
        }
        let ends_here = index + 1 == bytes.len() || bytes[index + 1].is_ascii_whitespace();
        if !ends_here {
            continue;
        }
        let candidate = text[..=index].trim();
        // The first punctuation mark wins only if the sentence is substantial;
        // otherwise keep reading (Node returned the whole text in that case).
        return (candidate.chars().count() >= 25).then(|| candidate.to_string());
    }
    None
}

/// Hard-cap with an ellipsis.
fn cap(text: &str) -> String {
    if text.chars().count() <= SHORT_CAP {
        return text.to_string();
    }
    let kept: String = text.chars().take(SHORT_CAP - 1).collect();
    format!("{}…", kept.trim_end())
}

/// Every entry recorded for a task, oldest first. Database only — the per-day
/// files cannot answer this.
pub fn task_history(db: &Db, task_id: i64) -> Vec<DailyLogEntry> {
    db.task_history(task_id)
}

/// Does this path look like a log file? Used by the viewer's file list.
pub fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(log_day_from_filename)
        .is_some()
}

#[cfg(test)]
mod tests;
