//! Ported from the Node app's `test/utils.test.js`, which pins these helpers
//! against real session behaviour. Where that suite leaned on the wall clock
//! this one pins an instant, so the boundaries are asserted exactly rather than
//! approached.

use super::*;
use chrono::TimeZone;

mod status;

#[test]
fn truncate_leaves_short_strings_alone() {
    assert_eq!(truncate("short", 20), "short");
}

#[test]
fn truncate_collapses_newlines_and_runs_of_whitespace() {
    assert_eq!(truncate("a\n\nb   c", 20), "a b c");
    assert_eq!(truncate("  padded  ", 20), "padded");
}

#[test]
fn truncate_appends_an_ellipsis_and_respects_the_width_budget() {
    let out = truncate(&"x".repeat(50), 10);
    assert_eq!(out.chars().count(), 10);
    assert_eq!(out.width(), 10);
    assert!(out.ends_with('…'));
}

#[test]
fn truncate_counts_display_width_not_code_points() {
    // CJK glyphs are two columns wide — a naive char-count slice would overflow
    // the pane and smear the border.
    let out = truncate(&"漢".repeat(20), 10);
    assert!(
        out.width() <= 10,
        "rendered width {} exceeds 10",
        out.width()
    );
    assert!(out.ends_with('…'));
}

#[test]
fn truncate_handles_empty_input_and_a_zero_budget() {
    assert_eq!(truncate("", 10), "");
    // Node produced a lone ellipsis for an impossible budget; preserved.
    assert_eq!(truncate("abcdef", 1), "…");
}

// --- formatContextUsage -----------------------------------------------------

#[test]
fn context_usage_is_a_percentage_of_the_window() {
    let half = CONTEXT_WINDOW / 2;
    let usage = Usage {
        input_tokens: Some(half),
        output_tokens: Some(0),
        ..Default::default()
    };
    assert_eq!(format_context_usage(Some(&usage)), "100K tokens (50%)");
}

#[test]
fn context_usage_counts_cache_reads_and_creations() {
    let usage = Usage {
        input_tokens: Some(1000),
        cache_read_input_tokens: Some(1000),
        cache_creation_input_tokens: Some(1000),
        output_tokens: Some(1000),
    };
    assert_eq!(format_context_usage(Some(&usage)), "4K tokens (2%)");
}

#[test]
fn empty_usage_renders_as_empty_not_as_a_zero() {
    assert_eq!(format_context_usage(None), "");
    assert_eq!(
        format_context_usage(Some(&Usage::default())),
        "0K tokens (0%)"
    );
}

// --- projectName / cwdToProjectDir ------------------------------------------

#[test]
fn project_name_uses_the_last_two_meaningful_segments() {
    assert_eq!(
        project_name("/Users/someone/dev/acme/portal"),
        "acme/portal"
    );
    assert_eq!(project_name("/Users/someone/dev/repo"), "someone/repo");
}

#[test]
fn project_name_falls_back_to_the_basename_for_shallow_paths() {
    assert_eq!(project_name("/repo"), "repo");
    assert_eq!(project_name(""), "unknown");
}

#[test]
fn cwd_encoding_matches_claude_codes_on_disk_format() {
    assert_eq!(cwd_to_project_dir("/Users/k/dev/app"), "-Users-k-dev-app");
    assert_eq!(
        cwd_to_project_dir("/Users/someone/dev/claude-sessions"),
        "-Users-someone-dev-claude-sessions"
    );
    // Worktrees are the realistic case: every separator becomes a dash, and the
    // dashes already in the path are left alone, so the encoding is not
    // reversible and must never be treated as if it were.
    assert_eq!(
        cwd_to_project_dir("/Users/someone/dev/claude-sessions-worktrees/task-4033"),
        "-Users-someone-dev-claude-sessions-worktrees-task-4033"
    );
}

// --- normaliseName ----------------------------------------------------------

#[test]
fn name_normalisation_ignores_case_spacing_and_punctuation() {
    // Shared by everything that matches a human-written project name against
    // something else a human wrote: ssh aliases, Optics keys, directory guesses.
    assert_eq!(normalise_name("NovaLink"), "novalink");
    assert_eq!(normalise_name("Or.bit.al"), "orbital");
    assert_eq!(normalise_name("atlas-backup"), "atlasbackup");
    assert_eq!(normalise_name("Task 4033"), "task4033");
}

#[test]
fn a_name_with_nothing_comparable_in_it_normalises_to_nothing() {
    // The callers treat this as "no match possible" rather than "matches
    // everything", which is the difference between opening nothing and opening
    // the wrong server.
    assert_eq!(normalise_name("!!!"), "");
    assert_eq!(normalise_name(""), "");
}

// --- timeAgo / formatStartTime ----------------------------------------------

#[test]
fn time_ago_renders_each_magnitude() {
    let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
    let ago = |secs: i64| time_ago(now - chrono::Duration::seconds(secs), now);
    assert_eq!(ago(5), "5s ago");
    assert_eq!(ago(59), "59s ago");
    assert_eq!(ago(60), "1m ago");
    assert_eq!(ago(300), "5m ago");
    assert_eq!(ago(3600), "1h ago");
    assert_eq!(ago(7200), "2h ago");
    assert_eq!(ago(3 * 86_400), "3d ago");
}

#[test]
fn activity_colour_ages_from_green_to_grey() {
    let now = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
    let at = |secs: i64| activity_color(now - chrono::Duration::seconds(secs), now);
    assert_eq!(at(59 * 60), Color::Green);
    assert_eq!(at(2 * 3600), Color::Yellow);
    assert_eq!(at(48 * 3600), Color::Gray);
}

#[test]
fn parse_timestamp_reads_transcript_timestamps() {
    assert!(parse_timestamp("2026-09-16T14:08:03.123Z").is_some());
    assert!(parse_timestamp("not a timestamp").is_none());
}

#[test]
fn start_time_renders_a_bare_clock_for_today() {
    let now = Local.with_ymd_and_hms(2026, 9, 16, 15, 0, 0).unwrap();
    assert_eq!(
        format_start_time("Wed Sep 16 14:08:03 2026", now),
        "02:08 PM"
    );
}

#[test]
fn start_time_prefixes_the_date_when_it_is_not_today() {
    let now = Local.with_ymd_and_hms(2026, 9, 16, 15, 0, 0).unwrap();
    // `ps` space-pads single-digit days.
    assert_eq!(
        format_start_time("Tue Sep  1 09:05:00 2026", now),
        "Sep 1 09:05 AM"
    );
}

#[test]
fn start_time_returns_the_raw_value_when_it_cannot_be_parsed() {
    let now = Local.with_ymd_and_hms(2026, 9, 16, 15, 0, 0).unwrap();
    assert_eq!(format_start_time("not a date", now), "not a date");
    assert_eq!(format_start_time("", now), "");
    assert_eq!(format_start_time("   ", now), "");
}

// --- detectSessionStatus ----------------------------------------------------
