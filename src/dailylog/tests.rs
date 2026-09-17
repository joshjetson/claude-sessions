//! Ported from the Node app's `test/dailylog.test.js`, plus the round-trip the
//! Node suite never had: a line written here must parse back through
//! [`crate::db::parse_log_line`], because the journal join and the log viewer
//! both read it that way.

use chrono::TimeZone;

use super::*;

// --- short_summary ----------------------------------------------------------

#[test]
fn prefers_the_fix_section_over_root_cause_and_before_after() {
    let md = [
        "**Root cause:** the redirect guard ran before session hydration.",
        "",
        "**Fix:** moved the guard after the session middleware.",
        "",
        "**Before vs after:** login looped; now it lands on the dashboard.",
    ]
    .join("\n");
    assert_eq!(
        short_summary(&md),
        "moved the guard after the session middleware."
    );
}

#[test]
fn matches_the_fix_heading_case_insensitively_and_without_the_colon() {
    assert_eq!(short_summary("**fix** did the thing."), "did the thing.");
    assert_eq!(short_summary("**FIX:** did the thing."), "did the thing.");
    assert_eq!(
        short_summary("**  Fix : ** did the thing."),
        "did the thing."
    );
}

#[test]
fn falls_back_to_the_whole_summary_when_there_is_no_fix_section() {
    assert_eq!(
        short_summary("Just a plain sentence about the work."),
        "Just a plain sentence about the work."
    );
}

#[test]
fn strips_markdown_punctuation_and_bullet_markers() {
    let out = short_summary("**Fix:**\n- changed `config.js`\n- removed *dead* code");
    assert!(
        !out.contains('*') && !out.contains('`'),
        "markdown leaked: {out}"
    );
    assert!(out.contains("changed config.js"), "{out}");
}

#[test]
fn collapses_to_the_first_sentence_when_it_is_substantial() {
    let out = short_summary(
        "**Fix:** Rewrote the retry loop to use exponential backoff. Then cleaned up logging.",
    );
    assert_eq!(out, "Rewrote the retry loop to use exponential backoff.");
}

#[test]
fn keeps_going_past_a_very_short_first_sentence() {
    let out = short_summary(
        "**Fix:** Done. Replaced the whole pagination helper with a cursor-based one.",
    );
    assert!(
        out.contains("pagination helper"),
        "dropped the substance: {out}"
    );
}

#[test]
fn hard_caps_long_output_with_an_ellipsis() {
    let out = short_summary(&format!("**Fix:** {}", "word ".repeat(100)));
    assert!(out.chars().count() <= 180, "length {}", out.chars().count());
    assert!(out.ends_with('…'));
}

#[test]
fn empty_input_yields_an_empty_string() {
    for input in ["", "   \n  "] {
        assert_eq!(short_summary(input), "");
    }
}

#[test]
fn a_fix_section_stops_at_the_next_heading_even_without_a_blank_line() {
    let out = short_summary("**Fix:** tightened the guard\n**Tested:** by hand");
    assert_eq!(out, "tightened the guard");
}

// --- writing ----------------------------------------------------------------

fn at(hour: u32, minute: u32) -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 9, 16, hour, minute, 0)
        .single()
        .expect("a real local time")
}

#[test]
fn a_written_line_has_the_documented_shape_and_parses_back() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let request = LogRequest {
        task_id: 5944,
        title: "Portal login redirect loop".into(),
        summary: "**Fix:** moved the guard after session hydration.".into(),
        mr_url: Some("https://example.com/group/repo/-/merge_requests/12".into()),
    };
    let path = append_daily_log(&paths, None, &request, at(9, 23)).unwrap();
    let body = fs::read_to_string(&path).unwrap();

    assert!(body.starts_with("# Daily log — 2026-09-16\n\n"), "{body}");
    let line = body.lines().last().unwrap();
    assert!(
        line.starts_with("- **#5944 Portal login redirect loop** — "),
        "{line}"
    );
    assert!(line.contains("([MR](https://example.com/group/repo/-/merge_requests/12))"));
    assert!(line.ends_with("_(09:23 AM)_"), "{line}");

    let parsed = parse_log_line(line).expect("a line we wrote must parse back");
    assert_eq!(parsed.task_id, 5944);
    assert_eq!(parsed.title, "Portal login redirect loop");
    assert_eq!(parsed.short, "moved the guard after session hydration.");
    assert_eq!(parsed.time, "09:23 AM");
}

#[test]
fn the_placeholder_file_is_replaced_and_later_entries_append() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let created = ensure_daily_log(&paths, "2026-09-16");
    assert!(fs::read_to_string(&created).unwrap().contains(PLACEHOLDER));

    for (id, title) in [(1, "First"), (2, "Second")] {
        append_daily_log(
            &paths,
            None,
            &LogRequest {
                task_id: id,
                title: title.into(),
                summary: "Did a thing that was worth doing here.".into(),
                mr_url: None,
            },
            at(10, 0),
        );
    }
    let body = fs::read_to_string(&created).unwrap();
    assert!(!body.contains(PLACEHOLDER), "placeholder survived: {body}");
    assert_eq!(body.matches("- **#").count(), 2, "{body}");
}

#[test]
fn a_summary_that_condenses_to_nothing_still_says_something() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let path = append_daily_log(
        &paths,
        None,
        &LogRequest {
            task_id: 7,
            title: "Quiet task".into(),
            summary: String::new(),
            mr_url: None,
        },
        at(14, 5),
    )
    .unwrap();
    assert!(fs::read_to_string(path).unwrap().contains("— Completed."));
}

#[test]
fn a_request_with_no_task_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    assert!(append_daily_log(&paths, None, &LogRequest::default(), at(9, 0)).is_none());
    assert!(!paths.logs_dir.join("2026-09-16.md").exists());
}

// --- reading ----------------------------------------------------------------

#[test]
fn list_log_dates_unions_the_files_and_the_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let db = Db::open(&paths);
    fs::create_dir_all(&paths.logs_dir).unwrap();
    fs::write(
        paths.logs_dir.join("2026-09-14.md"),
        "# Daily log — 2026-09-14\n",
    )
    .unwrap();
    fs::write(paths.logs_dir.join("notes.md"), "not a log").unwrap();
    db.put_daily_log_entry(&DailyLogEntry {
        day: "2026-09-12".into(),
        ts: "2026-09-12T10:00:00.000Z".into(),
        task_id: Some(1),
        title: "Older".into(),
        summary: String::new(),
        short: "Older".into(),
        mr_url: None,
    });

    // A day the file knows and a day only the row knows, oldest first — and
    // nothing that is not a dated log file.
    assert_eq!(
        list_log_dates(&paths, Some(&db)),
        vec!["2026-09-12".to_string(), "2026-09-14".to_string()]
    );
    assert_eq!(list_log_dates(&paths, None), vec!["2026-09-14".to_string()]);
}

#[test]
fn a_day_parses_into_entries_and_skips_the_heading() {
    let raw = "# Daily log — 2026-09-16\n\n\
        - **#1 One** — Did the first thing properly.  _(09:00 AM)_\n\
        some stray prose\n\
        - **#2 Two** — Did the second thing.  _(10:00 AM)_\n";
    let entries = parse_day("2026-09-16", raw);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].line.task_id, 1);
    assert_eq!(entries[1].line.title, "Two");
    assert_eq!(entries[1].day, "2026-09-16");
}

#[test]
fn reading_a_day_that_does_not_exist_is_empty_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    assert_eq!(read_log_for(&paths, "1999-01-01"), "");
    assert!(read_all_entries(&paths, None).is_empty());
}

#[test]
fn the_row_carries_the_full_summary_the_markdown_drops() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let db = Db::open(&paths);
    append_daily_log(
        &paths,
        Some(&db),
        &LogRequest {
            task_id: 6117,
            title: "Invoice totals".into(),
            summary: "**Root cause:** drift.\n\n**Fix:** round once at the total.".into(),
            mr_url: None,
        },
        at(11, 30),
    );
    let rows = db.task_history(6117);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].short, "round once at the total.");
    assert!(
        rows[0].summary.contains("Root cause"),
        "the full text is kept"
    );
    assert_eq!(rows[0].day, "2026-09-16");
}

#[test]
fn is_log_file_recognises_only_dated_markdown() {
    assert!(is_log_file(Path::new("/x/2026-09-16.md")));
    assert!(!is_log_file(Path::new("/x/notes.md")));
    assert!(!is_log_file(Path::new("/x/2026-09-16.txt")));
}
