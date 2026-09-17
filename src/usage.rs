//! Plan usage, for the header.
//!
//! Ported from the Node app's `src/usage.js`. There is no local file holding
//! it — Claude Code's `/usage` fetches at request time — so this shells out to
//! `claude -p "/usage"` and parses the reply.
//!
//! That call is itself a request against the quota it reports, so it is manual
//! by default and polled slowly when a `usage.intervalMinutes` is configured.
//! Checking your quota should not be a meaningful share of it.
//!
//! Everything except [`fetch_usage`] is pure text work, and [`fetch_usage`]
//! goes through [`Exec`], which goes through [`crate::term::SpawnPolicy`] — so
//! a test run cannot spend the real quota.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::term::Exec;
use crate::types::Color;
use crate::util::iso_now;

/// How long a `/usage` call may take. Node passed 60s; a slower answer is not
/// worth a hung refresh thread.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What one reading holds.
///
/// Missing metrics are `None`, NEVER `0` — "unknown" and "none used" must not
/// look the same in the header.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// False when the last refresh failed. The numbers are then the previous
    /// reading, and the readout says `(stale)`.
    pub ok: bool,
    pub session: Option<f64>,
    pub week: Option<f64>,
    pub fable: Option<f64>,
    pub session_resets: Option<String>,
    pub week_resets: Option<String>,
    /// When the reading was taken, ISO-8601.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UsageSnapshot {
    fn failed(error: String) -> Self {
        UsageSnapshot {
            ok: false,
            at: iso_now(),
            error: Some(error),
            ..UsageSnapshot::default()
        }
    }

    /// True when there is at least one number worth drawing.
    pub fn has_reading(&self) -> bool {
        self.session.is_some() || self.week.is_some()
    }

    /// Fold a fresh result into the previous one.
    ///
    /// A failure keeps the last numbers and marks them stale rather than
    /// blanking the readout — Node's `refreshUsage`. Better than the readout
    /// blinking out whenever a check times out.
    pub fn merge_over(self, previous: Option<&UsageSnapshot>) -> UsageSnapshot {
        if self.ok {
            return self;
        }
        let Some(previous) = previous else {
            return self;
        };
        UsageSnapshot {
            ok: false,
            error: self.error,
            at: self.at,
            ..previous.clone()
        }
    }

    /// The readout as plain text — what the width rules are asserted against.
    pub fn format(&self, available: usize) -> Option<String> {
        format_usage(self, available).map(|segments| {
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect()
        })
    }
}

/// Pull the percentages out of `/usage` output.
///
/// Expected shape, one metric per line:
/// ```text
/// Current session: 6% used · resets Sep 2 at 6:20pm (America/Chicago)
/// Current week (all models): 10% used · resets Sep 4 at 12pm (America/Chicago)
/// Current week (Fable): 0% used
/// ```
/// Anything it cannot find comes back `None` rather than `0`. The labels anchor
/// each match, so the "90% of your usage was at >150k context" line further
/// down is never read as a limit.
pub fn parse_usage(text: &str) -> UsageSnapshot {
    let lower = text.to_lowercase();
    let session = metric(&lower, text, "current session:");
    let week = metric(&lower, text, "current week (all models):");
    let fable = metric(&lower, text, "current week (fable):");
    UsageSnapshot {
        ok: true,
        session: session.as_ref().map(|m| m.percent),
        week: week.as_ref().map(|m| m.percent),
        fable: fable.as_ref().map(|m| m.percent),
        session_resets: session.and_then(|m| m.resets),
        week_resets: week.and_then(|m| m.resets),
        at: String::new(),
        error: None,
    }
}

struct Metric {
    percent: f64,
    resets: Option<String>,
}

/// `<label> N% used[ · resets <when>]`, read from the position `label` matched.
///
/// `lower` is the lowercased haystack used for the case-insensitive search and
/// `text` the original, so a captured reset time keeps its capitals.
fn metric(lower: &str, text: &str, label: &str) -> Option<Metric> {
    let at = lower.find(label)? + label.len();
    let rest = &text[at..];
    let cursor = skip_spaces(rest);
    let (digits, cursor) = take_number(cursor)?;
    let percent = digits.parse::<f64>().ok()?;
    let cursor = skip_spaces(cursor).strip_prefix('%')?;
    let cursor = skip_spaces(cursor);
    if !cursor.to_lowercase().starts_with("used") {
        return None;
    }
    Some(Metric {
        percent,
        resets: reset_time(&cursor[4..]),
    })
}

/// The ` · resets Sep 2 at 6:20pm` tail, without the `(timezone)` parenthetical.
fn reset_time(rest: &str) -> Option<String> {
    let rest = skip_spaces(rest).strip_prefix('·')?;
    let rest = skip_spaces(rest);
    if !rest.to_lowercase().starts_with("resets") {
        return None;
    }
    let rest = &rest[6..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let end = rest.find(['(', '\n']).unwrap_or(rest.len());
    let captured = rest[..end].trim();
    (!captured.is_empty()).then(|| captured.to_string())
}

fn skip_spaces(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

/// A leading `12` or `12.5`, and what follows it.
fn take_number(s: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    let bytes = s.as_bytes();
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 {
        return None;
    }
    if end < bytes.len() && bytes[end] == b'.' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    }
    Some((&s[..end], &s[end..]))
}

/// Colour for a percentage: green under 50, yellow to 75, red above.
///
/// `None` is grey — not knowing is not the same as being fine.
pub fn usage_color(percent: Option<f64>) -> Color {
    match percent {
        Some(p) if p.is_nan() => Color::Gray,
        Some(p) if p < 50.0 => Color::Green,
        Some(p) if p <= 75.0 => Color::Yellow,
        Some(_) => Color::Red,
        None => Color::Gray,
    }
}

/// A bar for a percentage: filled cells out of `width`, exactly `width` wide.
///
/// Rounds toward showing something — 1% reads as one filled cell rather than an
/// empty bar, and 99% keeps one empty cell rather than looking complete.
pub fn usage_bar(percent: Option<f64>, width: usize) -> String {
    let Some(percent) = percent.filter(|p| !p.is_nan()) else {
        return "░".repeat(width);
    };
    let clamped = percent.clamp(0.0, 100.0);
    let mut filled = (clamped / 100.0 * width as f64).round() as usize;
    if clamped > 0.0 && filled == 0 {
        filled = 1;
    }
    if clamped < 100.0 && filled == width {
        filled = width.saturating_sub(1);
    }
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// One coloured run of the readout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub color: Color,
}

/// The five forms, widest first. The first that fits `available` wins.
///
/// The header's left third is between 28 and 78 columns depending on terminal
/// width, so the readout degrades rather than colliding with the centred title.
/// `None` means "nothing fits" — which draws nothing at all, never a truncated
/// line.
pub fn format_usage(usage: &UsageSnapshot, available: usize) -> Option<Vec<Segment>> {
    let parts: Vec<(&str, &str, f64)> = [
        ("session", "ses", usage.session),
        ("week", "wk", usage.week),
    ]
    .into_iter()
    .filter_map(|(long, short, pct)| pct.map(|p| (long, short, p)))
    .collect();
    if parts.is_empty() {
        return None;
    }

    // (bar width, use the short label, wide separator)
    let forms: [(usize, bool, bool); 5] = [
        (10, false, true),
        (6, false, true),
        (5, true, false),
        (0, false, true),
        (0, false, false),
    ];
    for (index, (bar, short_label, wide_sep)) in forms.into_iter().enumerate() {
        let separator = if wide_sep { "  ·  " } else { " · " };
        // The last form drops the label as well as the bar: at that width the
        // percentage is the only information left worth the space.
        let bare = index == forms.len() - 1;
        let mut segments = vec![Segment {
            text: " ".to_string(),
            color: Color::Gray,
        }];
        for (position, (long, short, percent)) in parts.iter().enumerate() {
            if position > 0 {
                segments.push(Segment {
                    text: separator.to_string(),
                    color: Color::Gray,
                });
            }
            let label = if bare {
                String::new()
            } else if short_label {
                format!("{short} ")
            } else {
                format!("{long} ")
            };
            let bar = if bar > 0 {
                format!("{} ", usage_bar(Some(*percent), bar))
            } else {
                String::new()
            };
            segments.push(Segment {
                text: format!("{label}{bar}{}%", percent_text(*percent)),
                color: usage_color(Some(*percent)),
            });
        }
        if !usage.ok {
            segments.push(Segment {
                text: " (stale)".to_string(),
                color: Color::Gray,
            });
        }
        let width: usize = segments.iter().map(|s| s.text.chars().count()).sum();
        // Node measured the body and compared `width + 1 <= available`, the +1
        // being the leading space that is part of the segments here.
        if width <= available {
            return Some(segments);
        }
    }
    None
}

/// `6` not `6.0`, `12.5` as itself — what Node's template interpolation gave.
fn percent_text(percent: f64) -> String {
    if percent.fract() == 0.0 {
        format!("{}", percent as i64)
    } else {
        format!("{percent}")
    }
}

/// Ask Claude Code for the current usage. Never fails — a refusal, a missing
/// binary or an unparseable answer all come back as `ok: false`.
///
/// `home` is the working directory the check runs in: somewhere neutral, so a
/// repository's `CLAUDE.md` or settings cannot change what a usage check does.
pub fn fetch_usage(exec: &Exec, home: &Path, timeout: Duration) -> UsageSnapshot {
    let args = ["-p".to_string(), "/usage".to_string()];
    let output = exec.run_in("claude", &args, Some(home), timeout);
    if output.stdout.is_empty() {
        return UsageSnapshot::failed(output.failure_message());
    }
    let parsed = parse_usage(&output.stdout);
    if !parsed.has_reading() {
        return UsageSnapshot::failed("could not parse /usage output".to_string());
    }
    UsageSnapshot {
        at: iso_now(),
        ..parsed
    }
}

#[cfg(test)]
mod tests;
