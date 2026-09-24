//! The small pure helpers: display-width aware truncation, the formatting the
//! panes render, and the session status machine. The path ones — the
//! transcript-directory encoding, the project label and every directory
//! comparison — live in [`dir`] and are re-exported here, so a caller still
//! reaches for one module.
//!
//! Ported from the Node app's `src/utils.ts`. Nothing here reads the clock — the
//! caller passes `now` in — so the status machine and the "3m ago" strings are
//! testable without sleeping, and one render pass sees one consistent instant.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::types::{Color, EntryKind, LastEntry, SessionStatus, Usage};

mod dir;

pub use dir::{
    child_dir_of, cwd_to_project_dir, is_within_dir, join_dir, path_leaf, project_name, same_dir,
    trim_trailing_separators, SEPARATORS,
};

/// Claude Code's default context window, for the "72K tokens (36%)" readout.
pub const CONTEXT_WINDOW: u64 = 200_000;

/// The long-context window the 1M-token models run with.
pub const LARGE_CONTEXT_WINDOW: u64 = 1_000_000;

/// How long the agent may stay silent after the conversation last moved, with
/// no tool call in flight, before the session reads as idle.
///
/// That silence is the agent generating: thinking after a tool result, writing
/// a long reply, or streaming a large tool call whose line lands only when it
/// is complete. Each of those routinely passes a minute. Nothing in that state
/// waits on the person, so the old rule that turned it into "awaiting" after
/// thirty seconds was wrong every time a turn thought hard. The cap exists for
/// a session that hung or lost its connection: it stops a dead turn from
/// reading "working" forever.
pub const GENERATION_STALL: Duration = Duration::from_secs(10 * 60);

/// Reduce a human-written name to the part worth comparing: lowercase, letters
/// and digits only.
///
/// "NovaLink" and "novalink" are the same project; "Or.bit.al" and
/// "orbital" are the same host. Punctuation and spacing are how people write a
/// name, not what it is.
///
/// Lives here because four separate features match names this way — the ssh
/// host lookup, the Optics project lookup, the launch directory guess and the
/// purge filter — and the Node app wrote the same three lines in all four.
pub fn normalise_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// Whole-word substring test — what a `\bneedle\b` regex answered in Node.
///
/// The crate carries no regex dependency (one more transitive tree for
/// `cargo install` to resolve, for patterns this simple), so the handful of
/// word-boundary matches the port needs — purge stage names, the journal's
/// section classifier, prose-correction detection — go through here. A
/// "boundary" is the regex one: the character either side must not be
/// `[A-Za-z0-9_]`.
///
/// `needle` is matched literally, so callers lowercase both sides when they
/// want a case-insensitive match.
pub fn word_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        // Advance one character, not past the whole match: "aaa" must still
        // find the "aa" that starts one byte later.
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// True when any of `needles` matches as a whole word.
pub fn any_word_match(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| word_match(haystack, needle))
}

/// Collapse whitespace and cut to `max_len` *columns*, not characters.
///
/// Width rather than length because a pane budget is columns: a row of CJK
/// glyphs sliced by character count overflows its border and smears the frame.
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.is_empty() {
        return String::new();
    }
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.width() <= max_len {
        return flat;
    }
    // One column is spent on the ellipsis.
    let limit = max_len.saturating_sub(1);
    let mut out = String::new();
    let mut width = 0usize;
    for ch in flat.chars() {
        let w = ch.width().unwrap_or(0);
        if width + w > limit {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

/// Transcript and Odoo timestamps arrive as ISO strings. Callers that want a
/// rendered value parse first and decide for themselves what an unparseable
/// timestamp should look like.
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// "45s ago" / "12m ago" / "3h ago" / "2d ago" — one unit, always the largest
/// that fits.
pub fn time_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let diff = now - then;
    let (sec, min, hr, day) = (
        diff.num_seconds(),
        diff.num_minutes(),
        diff.num_hours(),
        diff.num_days(),
    );
    if sec < 60 {
        format!("{sec}s ago")
    } else if min < 60 {
        format!("{min}m ago")
    } else if hr < 24 {
        format!("{hr}h ago")
    } else {
        format!("{day}d ago")
    }
}

/// How stale a timestamp looks at a glance: fresh within the hour, aging within
/// the day, grey after that.
pub fn activity_color(then: DateTime<Utc>, now: DateTime<Utc>) -> Color {
    let hours = (now - then).num_seconds() as f64 / 3600.0;
    if hours < 1.0 {
        Color::Green
    } else if hours < 24.0 {
        Color::Yellow
    } else {
        Color::Gray
    }
}

/// Which window an occupancy of `total` tokens is measured against.
///
/// Node had one constant and measured everything against it, so a session on a
/// 1M-token model read `887K (444%)` — the token count was right, the
/// denominator was not. A session cannot hold more context than it was given,
/// so the smallest standard window that fits what the last message reported is
/// the window it is running with. Sessions inside 200K are unaffected, which is
/// every number the Node app ever printed correctly.
pub fn context_window(total: u64) -> u64 {
    if total > CONTEXT_WINDOW {
        LARGE_CONTEXT_WINDOW
    } else {
        CONTEXT_WINDOW
    }
}

/// How much of its context window a session is holding, rounded as Node
/// rounded it. Written once: the tree row and the conversation header both
/// print it, and they must never disagree.
pub fn context_percent(total: u64) -> i64 {
    (total as f64 / context_window(total) as f64 * 100.0).round() as i64
}

/// "72K tokens (36%)" for the session header, from the LAST message's usage —
/// the session's current occupancy, not the running total of everything it has
/// ever sent. Cache reads and cache creations count toward the window just as
/// much as fresh prompt tokens do.
pub fn format_context_usage(usage: Option<&Usage>) -> String {
    let Some(usage) = usage else {
        return String::new();
    };
    let total = usage.total_tokens();
    let thousands = (total as f64 / 1000.0).round() as u64;
    let pct = context_percent(total);
    format!("{thousands}K tokens ({pct}%)")
}

impl SessionStatus {
    /// The word shown in the session row. `AwaitingInput` renders the same
    /// as `Awaiting`. Nothing in this build produces it any more, but a daemon
    /// from an older release can still send it.
    pub fn label(self) -> &'static str {
        match self {
            SessionStatus::Working => "working",
            SessionStatus::Idle => "idle",
            SessionStatus::Awaiting | SessionStatus::AwaitingInput => "awaiting",
            SessionStatus::Compacting => "compacting",
            SessionStatus::Starting => "starting…",
        }
    }

    pub fn color(self) -> Color {
        match self {
            SessionStatus::Working => Color::Green,
            SessionStatus::Idle => Color::Gray,
            SessionStatus::Awaiting => Color::Yellow,
            SessionStatus::AwaitingInput | SessionStatus::Starting => Color::Cyan,
            SessionStatus::Compacting => Color::Magenta,
        }
    }
}

/// Tool names as a reader would say them. Unlisted tools (including every MCP
/// one) fall back to "using <Name>", which is why this is a match and not a
/// registry that has to be kept current.
fn friendly_tool_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "Read" => "reading",
        "Edit" => "editing",
        "Write" => "writing",
        "Bash" => "running command",
        "Grep" => "searching",
        "Glob" => "finding files",
        "Task" => "running agent",
        "WebFetch" => "fetching web",
        "WebSearch" => "searching web",
        "NotebookEdit" => "editing notebook",
        "AskUserQuestion" => "asking user",
        "EnterPlanMode" => "planning",
        _ => return None,
    })
}

fn using(name: &str) -> String {
    friendly_tool_name(name)
        .map(str::to_string)
        .unwrap_or_else(|| format!("using {name}"))
}

/// What the session is doing right now, for the second line of its row.
pub fn activity_label(last_entry: Option<&LastEntry>) -> String {
    let Some(entry) = last_entry else {
        return String::new();
    };
    match &entry.kind {
        EntryKind::Progress => {
            let progress = entry.progress.as_ref();
            match progress.and_then(|p| p.kind.as_deref()) {
                Some("bash_progress") => "running command".to_string(),
                Some("agent_progress") => "running agent".to_string(),
                Some("hook_progress") => {
                    // Hook names read "PreToolUse:Read" — the tool is the tail.
                    let hook = progress.and_then(|p| p.hook_name.as_deref()).unwrap_or("");
                    match hook.rsplit(':').next().unwrap_or("") {
                        "" => "processing".to_string(),
                        tool => using(tool),
                    }
                }
                _ => "processing".to_string(),
            }
        }
        EntryKind::Assistant if entry.has_message => match entry.tool_uses.last() {
            Some(name) => using(name),
            None => "responding".to_string(),
        },
        EntryKind::User => "thinking".to_string(),
        _ => String::new(),
    }
}

/// When the conversation last moved: the newest conversational timestamp the
/// transcript carries, or `fallback` (the file's mtime) when it carries none.
///
/// The daemon and the embedded scan both age a session through this, so the
/// two can never disagree about how old the same transcript is. The mtime is
/// only a fallback because bookkeeping lines touch it at moments unrelated to
/// the conversation.
pub fn activity_time(last_entry: Option<&LastEntry>, fallback: SystemTime) -> SystemTime {
    last_entry
        .and_then(LastEntry::activity_instant)
        .unwrap_or(fallback)
}

/// The status machine, driven by the transcript's newest conversational entry
/// and the time the conversation last moved (see [`activity_time`]). `now` is a
/// parameter so a whole refresh judges every session against one instant.
///
/// The rules, in order:
///
/// | Newest conversational entry                          | Status                                   |
/// |------------------------------------------------------|------------------------------------------|
/// | none                                                 | idle                                     |
/// | assistant `AskUserQuestion` / `ExitPlanMode` call    | awaiting                                 |
/// | a line that ends the turn (interrupt, stop-denial, local command output) | idle             |
/// | `system` (`turn_duration`, `compact_boundary`, `local_command`) | idle                          |
/// | `progress`                                           | working                                  |
/// | assistant with any other tool call                   | working, with no time limit              |
/// | user line (prompt, tool result, meta), or assistant prose / thinking | working for [`GENERATION_STALL`], then idle |
///
/// Only the transcript is read here. A permission prompt looks exactly like a
/// slow tool call in the transcript, so this function reports both as working.
/// The hook state in [`crate::hook_state`] tells them apart when it is
/// installed.
pub fn detect_session_status(
    last_entry: Option<&LastEntry>,
    activity_at: SystemTime,
    now: SystemTime,
) -> SessionStatus {
    let Some(entry) = last_entry else {
        return SessionStatus::Idle;
    };
    // A future stamp (clock skew, a copied transcript) reads as age zero.
    let age = now.duration_since(activity_at).unwrap_or(Duration::ZERO);

    // A question or a plan awaiting approval. It is unambiguous: the agent has
    // handed control back, so recency has nothing to add. With bookkeeping
    // lines ignored, the unanswered call itself is the newest conversational
    // entry until the answer lands as its tool result.
    if entry.awaits_user_decision() {
        return SessionStatus::Awaiting;
    }

    // The person ended the turn: an interrupt, a denial that told the agent to
    // stop, or the output of a local command. Nothing runs and nothing is
    // asked. This used to fall into the "user typed something" branch and read
    // "awaiting" forever.
    if entry.ends_turn {
        return SessionStatus::Idle;
    }

    match &entry.kind {
        // Only the subtypes that change whose turn it is reach this point (see
        // `Entry::drives_status`): a finished turn, a finished compaction, and
        // a local command's output. The agent owes nothing after any of them.
        EntryKind::System => SessionStatus::Idle,
        EntryKind::Progress => SessionStatus::Working,
        // A pending tool call is the agent working, with no time limit. A
        // build, a test suite, a browser step and a sleep loop all run for
        // minutes. A permission prompt looks the same here, and the hook state
        // is what tells the two apart.
        EntryKind::Assistant if entry.has_tool_use() => SessionStatus::Working,
        // Everything else means the agent owes the next line: it is thinking
        // after a prompt or a tool result, or it is still writing after prose
        // or a thinking block. A finished reply is followed by `turn_duration`
        // within a second, so prose that stays newest is mid-turn. Only a
        // silence past the stall cap reads as idle.
        EntryKind::User | EntryKind::Assistant => {
            if age < GENERATION_STALL {
                SessionStatus::Working
            } else {
                SessionStatus::Idle
            }
        }
        // Unreachable through the parser, which never projects a kind outside
        // the allowlist. A hand-built projection still gets an answer.
        EntryKind::Other(_) => SessionStatus::Idle,
    }
}

/// Render a process start time: bare clock time for today, with a date prefix
/// otherwise. The input is whatever `ps -o lstart` printed.
pub fn format_start_time(lstart: &str, now: DateTime<Local>) -> String {
    let raw = lstart.trim();
    if raw.is_empty() {
        return String::new();
    }
    let Some(started) = parse_start_time(raw) else {
        // Unparseable start times are shown as-is rather than hidden: a garbled
        // value in the row is a bug report, an empty cell is not.
        return raw.to_string();
    };
    let clock = started.format("%I:%M %p");
    if started.date() == now.date_naive() {
        clock.to_string()
    } else {
        format!("{} {}", started.format("%b %-d"), clock)
    }
}

/// A `ps -o lstart` stamp as an absolute instant, for arithmetic against file
/// timestamps (the pairing rules in [`crate::scan`] compare it to a
/// transcript's birth time).
///
/// `ps` prints local time with no zone, so this resolves through the local
/// zone; the earlier of the two readings is taken for the hour that repeats
/// when clocks go back.
pub fn start_time_instant(lstart: &str) -> Option<SystemTime> {
    let naive = parse_start_time(lstart.trim())?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(local.with_timezone(&Utc).into())
}

/// Parse a BSD `ps -o lstart` stamp: `Wed Sep 16 14:10:37 2026`, local time.
pub fn parse_start_time(raw: &str) -> Option<NaiveDateTime> {
    // BSD `ps -o lstart` prints "Tue Sep 16 14:08:03 2026" in local time; the
    // day is space-padded for single digits.
    NaiveDateTime::parse_from_str(raw, "%a %b %e %H:%M:%S %Y")
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|d| d.with_timezone(&Local).naive_local())
        })
}

/// Now, as the ISO-8601 stamp every stored timestamp uses.
///
/// Millisecond precision with a `Z` suffix — byte-for-byte what the Node app's
/// `new Date().toISOString()` wrote. Archive rows, daily-log rows and marker
/// files written by either implementation have to sort against each other, so
/// the format is fixed here once rather than spelled out at each call site.
pub fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
