//! One-time import of the pre-database state, and THE daily-log line parser.
//!
//! Everything under `~/.claude-sessions` was once loose files: a `meta.json` per
//! archived task and a markdown file per day. Those files are still written, so
//! this import only fills gaps and is safe to re-run on every daemon start —
//! which is exactly how it runs.
//!
//! [`parse_log_line`] lives here because the import is its first caller, but it
//! is the only implementation in the crate. The Node app wrote this parse three
//! times (the importer, the daily log, the log viewer) and they did not agree;
//! the viewer and the log writer both come through here instead.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{DailyLogEntry, Db, TaskArchive};
use crate::paths::Paths;

/// What the import found. Counts files handled, not rows created: re-running it
/// reports the same numbers while changing nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub tasks: usize,
    pub log_entries: usize,
    pub skipped: usize,
}

/// One parsed daily-log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub task_id: i64,
    pub title: String,
    pub short: String,
    pub mr_url: Option<String>,
    /// The wall-clock stamp as written, e.g. `10:24 AM`. The markdown never
    /// carried a date beyond the filename, so this is all there is.
    pub time: String,
}

/// `meta.json` as written by the archiver, read leniently: every field is
/// optional because these files predate the schema and unknown keys are
/// ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TaskMeta {
    task_id: Option<Value>,
    cwd: Option<String>,
    session_id: Option<String>,
    session_file: Option<String>,
    archived_at: Option<String>,
}

/// What `new Date(0).toISOString()` produced — the stamp the Node importer gave
/// an archive whose meta.json never recorded one.
const EPOCH: &str = "1970-01-01T00:00:00.000Z";

impl Db {
    /// Imports `tasks/*/meta.json` and `logs/*.md`. Idempotent: archives upsert
    /// on the task id, and a log line already in the table is skipped.
    pub fn import_existing_files(&self, paths: &Paths) -> ImportReport {
        let mut report = ImportReport::default();
        if !self.available() {
            return report;
        }
        self.import_task_archives(&paths.tasks_dir, &mut report);
        self.import_daily_logs(&paths.logs_dir, &mut report);
        report
    }

    fn import_task_archives(&self, tasks_dir: &Path, report: &mut ImportReport) {
        for dir in sorted_entries(tasks_dir) {
            let meta_path = tasks_dir.join(&dir).join("meta.json");
            let Ok(text) = fs::read_to_string(&meta_path) else {
                // No meta.json at all is not a skip — the directory may hold
                // nothing but a transcript, or not be a task directory.
                continue;
            };
            let Ok(meta) = serde_json::from_str::<TaskMeta>(&text) else {
                report.skipped += 1;
                continue;
            };
            // The directory is named after the task, so it is the fallback when
            // the file itself never recorded an id.
            let Some(task_id) = task_id_of(meta.task_id.as_ref(), &dir) else {
                report.skipped += 1;
                continue;
            };
            self.put_task_archive(&TaskArchive {
                task_id,
                cwd: meta.cwd.unwrap_or_default(),
                session_id: meta.session_id.unwrap_or_default(),
                session_file: meta.session_file.unwrap_or_default(),
                archived_at: meta.archived_at.unwrap_or_else(|| EPOCH.to_string()),
            });
            report.tasks += 1;
        }
    }

    fn import_daily_logs(&self, logs_dir: &Path, report: &mut ImportReport) {
        // Everything the table already holds, so a re-import adds nothing.
        let known: HashSet<String> = self
            .daily_log_entries(None)
            .iter()
            .map(|entry| entry_key(&entry.day, entry.task_id, &entry.ts))
            .collect();

        for name in sorted_entries(logs_dir) {
            let Some(day) = log_day_from_filename(&name) else {
                continue;
            };
            let Ok(body) = fs::read_to_string(logs_dir.join(&name)) else {
                continue;
            };
            let mut seq = 0usize;
            for line in body.lines() {
                if !line.starts_with("- **#") {
                    continue;
                }
                let Some(parsed) = parse_log_line(line) else {
                    report.skipped += 1;
                    continue;
                };
                // The markdown keeps only a wall-clock time, and not even a
                // timezone. Synthesise a stable ordering key from the day plus
                // the line's position so a re-import lands on the same value.
                let ts = format!("{day}T00:00:{seq:02}.000Z");
                seq += 1;
                if known.contains(&entry_key(day, Some(parsed.task_id), &ts)) {
                    continue;
                }
                self.put_daily_log_entry(&DailyLogEntry {
                    day: day.to_string(),
                    ts,
                    task_id: Some(parsed.task_id),
                    title: parsed.title,
                    // The full sign-off was never kept in the markdown.
                    summary: String::new(),
                    short: parsed.short,
                    mr_url: parsed.mr_url,
                });
                report.log_entries += 1;
            }
        }
    }
}

/// Parses one daily-log line back into fields.
///
/// The written form is
/// `- **#<id> <title>** — <short> ([MR](<url>))  _(hh:mm AM)_`,
/// where the MR link is optional. Titles routinely contain an em dash of their
/// own, so the split is on the `**` that closes the bold run and the dash that
/// follows it — never on the first dash in the line.
///
/// Returns `None` for anything that is not an entry (headings, blank lines,
/// plain bullets) rather than half-parsing it.
pub fn parse_log_line(line: &str) -> Option<LogLine> {
    let rest = line.trim_end();
    let rest = skip_whitespace(rest.strip_prefix('-')?)?;
    let rest = rest.strip_prefix("**#")?;
    let (digits, rest) = split_digits(rest)?;
    let task_id = digits.parse::<i64>().ok()?;
    let rest = skip_whitespace(rest)?;
    let (title, rest) = split_title(rest)?;
    let (body, time) = split_time(rest)?;
    let (short, mr_url) = split_mr_link(body);
    Some(LogLine {
        task_id,
        title: title.trim().to_string(),
        short: short.trim().to_string(),
        mr_url,
        time: time.to_string(),
    })
}

/// The day a `YYYY-MM-DD.md` log file is named after, or `None` for anything
/// else in the directory.
pub fn log_day_from_filename(name: &str) -> Option<&str> {
    let day = name.strip_suffix(".md")?;
    let bytes = day.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    shaped.then_some(day)
}

/// The title runs to the `**` that is followed by the ` — ` separator — not to
/// the first `**`, so a title that bolds a word of its own still parses.
fn split_title(s: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(offset) = s[from..].find("**") {
        let at = from + offset;
        if let Some(after) = skip_separator(&s[at + 2..]) {
            return Some((&s[..at], after));
        }
        from = at + 2;
    }
    None
}

/// The ` — ` between the bolded title and the summary: whitespace, an em dash,
/// whitespace. Anything else means this `**` was not the closing one.
fn skip_separator(s: &str) -> Option<&str> {
    skip_whitespace(skip_whitespace(s)?.strip_prefix('—')?)
}

/// Splits the trailing `_(hh:mm AM)_` stamp off the end of the line, returning
/// what came before it and the stamp itself.
fn split_time(s: &str) -> Option<(&str, &str)> {
    let head = s.strip_suffix(")_")?;
    let at = head.find("_(")?;
    let time = &head[at + 2..];
    (!time.is_empty()).then_some((&head[..at], time))
}

/// Splits an optional trailing `([MR](url))` off the summary. A summary that
/// merely ends in brackets keeps them.
fn split_mr_link(body: &str) -> (&str, Option<String>) {
    const OPEN: &str = "([MR](";
    let trimmed = body.trim_end();
    let Some(at) = trimmed.rfind(OPEN) else {
        return (body, None);
    };
    let Some(url) = trimmed[at + OPEN.len()..].strip_suffix("))") else {
        return (body, None);
    };
    if url.is_empty() || url.contains(char::is_whitespace) {
        return (body, None);
    }
    (&body[..at], Some(url.to_string()))
}

/// At least one whitespace character, consumed. `None` when there is none —
/// the written form always separates these fields.
fn skip_whitespace(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    (trimmed.len() != s.len()).then_some(trimmed)
}

fn split_digits(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (end > 0).then(|| s.split_at(end))
}

/// `taskId` may be a number or a string in these files, and may be missing
/// entirely — in which case the directory name is the id.
fn task_id_of(value: Option<&Value>, dir_name: &str) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => dir_name.trim().parse().ok(),
    }
}

/// Identity of a log row for the purposes of "have I imported this already".
fn entry_key(day: &str, task_id: Option<i64>, ts: &str) -> String {
    match task_id {
        Some(id) => format!("{day}|{id}|{ts}"),
        None => format!("{day}|none|{ts}"),
    }
}

/// Directory entries by name. Sorted so an import is reproducible — the
/// synthesised log timestamps depend on the order lines are seen in.
fn sorted_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}
