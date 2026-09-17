//! Ported from the Node app's `test/usage.test.js`.
//!
//! The header assertions there drove an Ink render; here they exercise the same
//! decisions directly (which form fits, whether the title would move) and the
//! rendered-header cases live in `ui::tests::layout`.

use super::*;

const REAL_OUTPUT: &str =
    "You are currently using your subscription to power your Claude Code usage

Current session: 6% used · resets Sep 2 at 6:20pm (America/Chicago)
Current week (all models): 10% used · resets Sep 4 at 12pm (America/Chicago)
Current week (Fable): 0% used

What's contributing to your limits usage?
Last 24h · 1007 requests · 17 sessions
  90% of your usage was at >150k context
";

// --- parse_usage ------------------------------------------------------------

#[test]
fn reads_the_three_percentages_from_real_output() {
    let u = parse_usage(REAL_OUTPUT);
    assert_eq!(u.session, Some(6.0));
    assert_eq!(u.week, Some(10.0));
    assert_eq!(u.fable, Some(0.0));
}

#[test]
fn captures_the_reset_times_without_the_timezone_parenthetical() {
    let u = parse_usage(REAL_OUTPUT);
    assert_eq!(u.session_resets.as_deref(), Some("Sep 2 at 6:20pm"));
    assert_eq!(u.week_resets.as_deref(), Some("Sep 4 at 12pm"));
}

#[test]
fn is_not_confused_by_the_percentages_further_down_the_output() {
    // "90% of your usage was at >150k context" must not be read as a limit.
    assert_eq!(parse_usage(REAL_OUTPUT).session, Some(6.0));
}

#[test]
fn handles_a_decimal_percentage() {
    assert_eq!(
        parse_usage("Current session: 12.5% used").session,
        Some(12.5)
    );
}

#[test]
fn missing_metrics_are_none_not_zero() {
    // "unknown" and "none used" must not look the same in the header.
    let u = parse_usage("Current session: 6% used");
    assert_eq!(u.session, Some(6.0));
    assert_eq!(u.week, None);
    assert_eq!(u.fable, None);
}

#[test]
fn unparseable_input_yields_nones_rather_than_panicking() {
    for input in ["", "something else entirely", "Current session: used"] {
        let u = parse_usage(input);
        assert_eq!(u.session, None, "{input:?}");
        assert_eq!(u.week, None, "{input:?}");
        assert!(!u.has_reading());
    }
}

#[test]
fn a_metric_with_no_reset_tail_still_parses() {
    let u = parse_usage("Current week (all models): 33% used\n");
    assert_eq!(u.week, Some(33.0));
    assert_eq!(u.week_resets, None);
}

#[test]
fn the_labels_are_matched_case_insensitively() {
    assert_eq!(parse_usage("CURRENT SESSION: 8% USED").session, Some(8.0));
}

// --- usage_color ------------------------------------------------------------

#[test]
fn green_below_fifty() {
    for p in [0.0, 1.0, 25.0, 49.0, 49.9] {
        assert_eq!(usage_color(Some(p)), Color::Green, "{p}%");
    }
}

#[test]
fn yellow_from_fifty_through_seventy_five() {
    for p in [50.0, 60.0, 75.0] {
        assert_eq!(usage_color(Some(p)), Color::Yellow, "{p}%");
    }
}

#[test]
fn red_above_seventy_five() {
    for p in [75.1, 76.0, 90.0, 100.0, 130.0] {
        assert_eq!(usage_color(Some(p)), Color::Red, "{p}%");
    }
}

#[test]
fn no_reading_is_grey_not_green() {
    // Grey must not be mistaken for "plenty left".
    assert_eq!(usage_color(None), Color::Gray);
    assert_eq!(usage_color(Some(f64::NAN)), Color::Gray);
}

// --- usage_bar --------------------------------------------------------------

#[test]
fn the_bar_fills_proportionally() {
    assert_eq!(usage_bar(Some(0.0), 10), "░░░░░░░░░░");
    assert_eq!(usage_bar(Some(50.0), 10), "█████░░░░░");
    assert_eq!(usage_bar(Some(100.0), 10), "██████████");
}

#[test]
fn one_percent_shows_something_rather_than_an_empty_bar() {
    assert_eq!(usage_bar(Some(1.0), 10), "█░░░░░░░░░");
}

#[test]
fn ninety_nine_percent_keeps_an_empty_cell_rather_than_looking_finished() {
    assert_eq!(usage_bar(Some(99.0), 10), "█████████░");
}

#[test]
fn an_unknown_percentage_is_an_empty_bar() {
    assert_eq!(usage_bar(None, 6), "░░░░░░");
}

#[test]
fn out_of_range_values_are_clamped() {
    assert_eq!(usage_bar(Some(-5.0), 5), "░░░░░");
    assert_eq!(usage_bar(Some(150.0), 5), "█████");
}

#[test]
fn the_bar_is_exactly_the_requested_width() {
    for p in [0.0, 33.0, 67.0, 100.0] {
        assert_eq!(usage_bar(Some(p), 8).chars().count(), 8, "{p}%");
    }
}

// --- format_usage -----------------------------------------------------------

fn reading(session: Option<f64>, week: Option<f64>, ok: bool) -> UsageSnapshot {
    UsageSnapshot {
        ok,
        session,
        week,
        ..UsageSnapshot::default()
    }
}

#[test]
fn a_wide_header_gets_full_labels_and_ten_cell_bars() {
    let out = reading(Some(63.0), Some(52.0), true).format(60).unwrap();
    assert!(out.contains("session ██████"), "{out}");
    assert!(out.contains(" 63%"), "{out}");
    assert!(out.contains("week █████"), "{out}");
    assert!(out.contains(" 52%"), "{out}");
}

#[test]
fn it_never_exceeds_the_space_it_was_given() {
    // Overflowing would push into the centred title.
    let usage = reading(Some(63.0), Some(52.0), true);
    for width in [80, 60, 48, 38, 30, 24, 14, 10] {
        if let Some(out) = usage.format(width) {
            assert!(
                out.chars().count() <= width,
                "{width} cols produced {} chars: {out}",
                out.chars().count()
            );
        }
    }
}

#[test]
fn bars_are_dropped_before_the_numbers_are() {
    // The percentage is the information; the bar is the glanceable extra.
    let narrow = reading(Some(63.0), Some(52.0), true).format(26).unwrap();
    assert!(narrow.contains("63%"), "{narrow}");
    assert!(narrow.contains("52%"), "{narrow}");
    assert!(
        !narrow.contains('█'),
        "kept bars at a width where they do not fit: {narrow}"
    );
}

#[test]
fn too_little_room_yields_nothing_rather_than_a_broken_line() {
    assert_eq!(reading(Some(63.0), Some(52.0), true).format(4), None);
}

#[test]
fn short_labels_are_readable_not_truncated_words() {
    let usage = reading(Some(63.0), Some(52.0), true);
    let mid = usage.format(34).unwrap();
    if mid.contains("ses") {
        assert!(
            !mid.contains("wee "),
            "used \"wee\" instead of \"wk\": {mid}"
        );
    }
}

#[test]
fn nothing_read_yet_draws_nothing() {
    assert_eq!(UsageSnapshot::default().format(80), None);
    assert_eq!(reading(None, None, true).format(80), None);
}

#[test]
fn a_partial_reading_renders_what_it_has() {
    let out = reading(Some(88.0), None, true).format(80).unwrap();
    assert!(out.contains("88%"), "{out}");
    assert!(!out.contains("week"), "{out}");
}

#[test]
fn a_failed_refresh_keeps_the_last_numbers_and_marks_them_stale() {
    let previous = UsageSnapshot {
        ok: true,
        session: Some(42.0),
        week: Some(55.0),
        session_resets: Some("Sep 2 at 6:20pm".into()),
        ..UsageSnapshot::default()
    };
    let failed = UsageSnapshot {
        ok: false,
        error: Some("claude did not answer".into()),
        at: "2026-09-16T00:00:00.000Z".into(),
        ..UsageSnapshot::default()
    };
    let merged = failed.merge_over(Some(&previous));
    assert_eq!(merged.session, Some(42.0));
    assert_eq!(merged.week, Some(55.0));
    assert_eq!(merged.session_resets.as_deref(), Some("Sep 2 at 6:20pm"));
    assert!(!merged.ok);
    assert_eq!(merged.error.as_deref(), Some("claude did not answer"));

    let out = merged.format(80).unwrap();
    assert!(out.contains("42%"), "{out}");
    assert!(out.contains("(stale)"), "{out}");
}

#[test]
fn a_successful_refresh_replaces_the_previous_reading_entirely() {
    let previous = reading(Some(42.0), Some(55.0), true);
    let fresh = reading(Some(7.0), None, true).merge_over(Some(&previous));
    assert_eq!(fresh.session, Some(7.0));
    assert_eq!(fresh.week, None, "a fresh reading is not merged field-wise");
}

#[test]
fn a_first_failure_with_no_previous_reading_shows_nothing() {
    let failed = UsageSnapshot {
        ok: false,
        error: Some("nope".into()),
        ..UsageSnapshot::default()
    };
    assert_eq!(failed.merge_over(None).format(80), None);
}

#[test]
fn each_percentage_carries_its_own_colour() {
    let segments = format_usage(&reading(Some(10.0), Some(90.0), true), 80).unwrap();
    let coloured: Vec<Color> = segments
        .iter()
        .filter(|s| s.text.contains('%'))
        .map(|s| s.color)
        .collect();
    assert_eq!(coloured, vec![Color::Green, Color::Red]);
}

#[test]
fn a_decimal_percentage_keeps_its_decimal_and_a_whole_one_does_not_gain_zero() {
    let out = reading(Some(12.5), Some(6.0), true).format(80).unwrap();
    assert!(out.contains("12.5%"), "{out}");
    assert!(out.contains(" 6%") && !out.contains("6.0%"), "{out}");
}

// --- fetch_usage ------------------------------------------------------------

#[test]
fn fetching_under_a_refusing_spawn_policy_fails_rather_than_spending_quota() {
    // The whole point of the guard: a test run must never make a real request
    // against the user's plan.
    let exec = Exec::new(crate::term::SpawnPolicy::Refuse);
    let result = fetch_usage(&exec, std::path::Path::new("/"), Duration::from_secs(1));
    assert!(!result.ok);
    assert!(
        result.error.unwrap_or_default().contains("Refusing to run"),
        "the refusal should say why"
    );
    assert_eq!(result.session, None, "a refusal is not a reading of zero");
}
