//! The one-time import, and the daily-log line parser it is the first caller of.

use super::*;
use std::fs;

// --- parse_log_line ---------------------------------------------------------

#[test]
fn parses_a_full_line_with_an_mr_link() {
    let parsed = parse_log_line(
        "- **#5944 Portal login redirect loop** — Moved the guard after session hydration. ([MR](https://git/x/-/merge_requests/12))  _(10:24 AM)_",
    )
    .unwrap();
    assert_eq!(parsed.task_id, 5944);
    assert_eq!(parsed.title, "Portal login redirect loop");
    assert_eq!(parsed.short, "Moved the guard after session hydration.");
    assert_eq!(
        parsed.mr_url.as_deref(),
        Some("https://git/x/-/merge_requests/12")
    );
    assert_eq!(parsed.time, "10:24 AM");
}

#[test]
fn parses_a_line_with_no_mr_link() {
    let parsed =
        parse_log_line("- **#6117 Invoice totals** — Round once at the total.  _(02:00 PM)_")
            .unwrap();
    assert_eq!(parsed.task_id, 6117);
    assert_eq!(parsed.mr_url, None);
    assert_eq!(parsed.short, "Round once at the total.");
    assert_eq!(parsed.time, "02:00 PM");
}

#[test]
fn titles_containing_an_em_dash_still_split_correctly() {
    let parsed =
        parse_log_line("- **#42 Fix the thing — really** — Did it.  _(09:00 AM)_").unwrap();
    assert_eq!(parsed.task_id, 42);
    assert_eq!(parsed.title, "Fix the thing — really");
    assert_eq!(parsed.short, "Did it.", "split on the wrong dash");
}

#[test]
fn a_summary_containing_an_em_dash_survives_too() {
    let parsed =
        parse_log_line("- **#43 Totals** — Rounded once — at the total.  _(09:00 AM)_").unwrap();
    assert_eq!(parsed.title, "Totals");
    assert_eq!(parsed.short, "Rounded once — at the total.");
}

#[test]
fn a_title_that_bolds_a_word_of_its_own_still_parses() {
    // The closing ** is the one followed by the separator, not the first one.
    let parsed =
        parse_log_line("- **#44 Fix **all** the things** — Did them.  _(09:00 AM)_").unwrap();
    assert_eq!(parsed.title, "Fix **all** the things");
    assert_eq!(parsed.short, "Did them.");
}

#[test]
fn a_summary_ending_in_brackets_is_not_read_as_an_mr_link() {
    let parsed = parse_log_line("- **#45 Totals** — Rounded (twice, sadly)  _(09:00 AM)_").unwrap();
    assert_eq!(parsed.short, "Rounded (twice, sadly)");
    assert_eq!(parsed.mr_url, None);
}

#[test]
fn non_entry_lines_are_rejected_rather_than_half_parsed() {
    for line in [
        "# Daily log — 2026-08-01",
        "",
        "- just a bullet",
        "- **#not-a-number Title** — text  _(09:00 AM)_",
        "- **#42 Missing the stamp** — text",
        "- **#42 No separator** text  _(09:00 AM)_",
    ] {
        assert_eq!(parse_log_line(line), None, "should not parse: {line:?}");
    }
}

/// The awkward shapes, pinned to what the Node regex actually produced — these
/// were diffed line for line against `src/db-migrate.ts` rather than guessed,
/// because the log viewer and the log writer both parse through here now.
#[test]
fn the_awkward_shapes_parse_the_way_the_node_regex_did() {
    /// `(task_id, title, short, mr_url, time)` — a parse, as a table row.
    type Fields = (
        i64,
        &'static str,
        &'static str,
        Option<&'static str>,
        &'static str,
    );

    let cases: [(&str, Option<Fields>); 6] = [
        // Whitespace around every separator collapses.
        (
            "-    **#7 Extra spaces**    —    Lots of space.    _(11:11 AM)_",
            Some((7, "Extra spaces", "Lots of space.", None, "11:11 AM")),
        ),
        // An empty title and an empty summary are both legal.
        (
            "- **#8 ** — Empty-ish title.  _(01:02 PM)_",
            Some((8, "", "Empty-ish title.", None, "01:02 PM")),
        ),
        (
            "- **#9 Empty summary** —   _(01:02 PM)_",
            Some((9, "Empty summary", "", None, "01:02 PM")),
        ),
        // An MR link with no URL is not a link; the text stays in the summary.
        (
            "- **#11 No url** — Done. ([MR]())  _(03:00 PM)_",
            Some((11, "No url", "Done. ([MR]())", None, "03:00 PM")),
        ),
        // Nor is one whose URL contains whitespace.
        (
            "- **#17 Spacey url** — Done. ([MR](https://git/a b))  _(08:00 PM)_",
            Some((
                17,
                "Spacey url",
                "Done. ([MR](https://git/a b))",
                None,
                "08:00 PM",
            )),
        ),
        // Multi-byte text must not be sliced mid-character.
        (
            "- **#16 Unicode — 日本語 title** — 変更しました。  _(07:00 PM)_",
            Some((
                16,
                "Unicode — 日本語 title",
                "変更しました。",
                None,
                "07:00 PM",
            )),
        ),
    ];

    for (line, want) in cases {
        let got = parse_log_line(line).map(|p| {
            (
                p.task_id,
                p.title.clone(),
                p.short.clone(),
                p.mr_url.clone(),
                p.time.clone(),
            )
        });
        let want = want.map(|(id, title, short, mr, time)| {
            (
                id,
                title.to_string(),
                short.to_string(),
                mr.map(str::to_string),
                time.to_string(),
            )
        });
        assert_eq!(got, want, "line: {line:?}");
    }
}

#[test]
fn only_dated_markdown_files_are_log_days() {
    assert_eq!(log_day_from_filename("2026-08-01.md"), Some("2026-08-01"));
    for name in [
        "2026-08-01.txt",
        "notes.md",
        "2026-8-1.md",
        "2026-08-01",
        "20260801.md",
    ] {
        assert_eq!(
            log_day_from_filename(name),
            None,
            "should not be a day: {name}"
        );
    }
}

// --- importExistingFiles ----------------------------------------------------

fn write_meta(t: &TestDb, dir: &str, body: &str) {
    let path = t.paths.tasks_dir.join(dir);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("meta.json"), body).unwrap();
}

fn write_log(t: &TestDb, name: &str, body: &str) {
    fs::create_dir_all(&t.paths.logs_dir).unwrap();
    fs::write(t.paths.logs_dir.join(name), body).unwrap();
}

#[test]
fn imports_task_archives_and_log_lines() {
    let t = open();
    write_meta(
        &t,
        "5944",
        r#"{"taskId":5944,"cwd":"/repo/portal","sessionId":"sess-a",
            "sessionFile":"/x/sess-a.jsonl","archivedAt":"2026-07-23T10:00:00.000Z"}"#,
    );
    write_log(
        &t,
        "2026-08-01.md",
        "# Daily log — 2026-08-01\n\n\
         - **#5944 Portal login** — Fixed the guard. ([MR](https://git/x/1))  _(10:24 AM)_\n\
         - **#6117 Invoice totals** — Round once.  _(02:00 PM)_\n",
    );

    let report = t.db.import_existing_files(&t.paths);
    assert_eq!(report.tasks, 1);
    assert_eq!(report.log_entries, 2);
    assert_eq!(report.skipped, 0);

    assert_eq!(
        t.db.get_task_archive(5944).unwrap().session_file,
        "/x/sess-a.jsonl"
    );
    let rows = t.db.daily_log_entries(Some("2026-08-01"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].task_id, Some(5944));
    assert_eq!(rows[0].mr_url.as_deref(), Some("https://git/x/1"));
    // The markdown never carried the full sign-off.
    assert_eq!(rows[0].summary, "");
}

#[test]
fn importing_twice_changes_nothing() {
    let t = open();
    write_meta(&t, "5944", r#"{"taskId":5944,"sessionId":"sess-a"}"#);
    write_log(
        &t,
        "2026-08-01.md",
        "- **#5944 Portal login** — Fixed the guard.  _(10:24 AM)_\n",
    );

    let first = t.db.import_existing_files(&t.paths);
    let second = t.db.import_existing_files(&t.paths);

    // The report counts files handled, so it repeats…
    assert_eq!(first.tasks, second.tasks);
    assert_eq!(second.log_entries, 0, "a second import must add no rows");
    // …but the store is unchanged.
    assert_eq!(t.db.list_archived_task_ids(), vec![5944]);
    assert_eq!(t.db.daily_log_entries(None).len(), 1);
}

#[test]
fn a_meta_file_without_a_task_id_falls_back_to_its_directory() {
    let t = open();
    write_meta(&t, "6117", r#"{"sessionId":"sess-c","cwd":"/repo/api"}"#);

    assert_eq!(t.db.import_existing_files(&t.paths).tasks, 1);
    let row = t.db.get_task_archive(6117).unwrap();
    assert_eq!(row.session_id, "sess-c");
    // Nothing recorded when it was archived, so the epoch stands in.
    assert_eq!(row.archived_at, "1970-01-01T00:00:00.000Z");
}

#[test]
fn unreadable_and_unnamed_entries_are_counted_not_fatal() {
    let t = open();
    write_meta(&t, "5944", r#"{"taskId":5944}"#);
    write_meta(&t, "broken", "{ this is not json");
    write_meta(&t, "not-a-task", r#"{"sessionId":"sess-x"}"#);
    // A directory with no meta.json at all is simply not a task archive.
    fs::create_dir_all(t.paths.tasks_dir.join("9999")).unwrap();
    write_log(
        &t,
        "2026-08-01.md",
        "- **#5944 Good line** — Fine.  _(10:24 AM)_\n\
         - **#not-a-number Bad line** — Broken.  _(10:25 AM)_\n\
         some prose that is not an entry\n",
    );

    let report = t.db.import_existing_files(&t.paths);
    assert_eq!(report.tasks, 1);
    assert_eq!(report.log_entries, 1);
    assert_eq!(report.skipped, 3, "two bad metas and one bad line");
    assert_eq!(t.db.list_archived_task_ids(), vec![5944]);
}

#[test]
fn missing_directories_are_not_an_error() {
    let t = open();
    assert_eq!(
        t.db.import_existing_files(&t.paths),
        ImportReport::default()
    );
}

#[test]
fn entries_appended_after_an_import_are_not_re_imported() {
    // The live daily log writes a real ISO timestamp; the import synthesises one
    // from the line's position. They must not collide into a duplicate.
    let t = open();
    write_log(
        &t,
        "2026-08-01.md",
        "- **#5944 Portal login** — Fixed the guard.  _(10:24 AM)_\n",
    );
    t.db.import_existing_files(&t.paths);
    t.db.put_daily_log_entry(&log_entry(
        "2026-08-01",
        "2026-08-01T15:00:00.000Z",
        6117,
        "Later",
    ));

    assert_eq!(t.db.import_existing_files(&t.paths).log_entries, 0);
    assert_eq!(t.db.daily_log_entries(Some("2026-08-01")).len(), 2);
}
