//! Fitting math, wrapping, pane geometry and the header overlay.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::ui::components::{keep_visible, render_header, split, HEADER_H, MIN_PANE_COLS};
use crate::ui::spans::{clip_text, drop_leading, fit_spans, spans_width, wrap_spans};
use crate::ui::state::{Stats, UsageReadout};
use crate::ui::tests::{render, text};

fn plain(s: &str) -> Vec<Span<'static>> {
    vec![Span::raw(s.to_string())]
}

#[test]
fn clip_never_splits_a_wide_glyph() {
    // A CJK glyph is two columns: a three-column budget fits one glyph and one
    // ASCII character, never half a glyph.
    assert_eq!(clip_text("日本語", 3), "日");
    assert_eq!(clip_text("日本語", 4), "日本");
    assert_eq!(clip_text("日a", 3), "日a");
}

#[test]
fn clip_of_zero_width_is_empty() {
    assert_eq!(clip_text("anything", 0), "");
}

#[test]
fn fit_pads_short_runs_to_exactly_the_width() {
    let fitted = fit_spans(plain("hi"), 6, Style::default());
    assert_eq!(spans_width(&fitted), 6);
    assert_eq!(
        fitted
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>(),
        "hi    "
    );
}

#[test]
fn fit_leaves_an_exact_run_untouched() {
    let fitted = fit_spans(plain("abcdef"), 6, Style::default());
    assert_eq!(fitted.len(), 1);
    assert_eq!(fitted[0].content, "abcdef");
}

#[test]
fn fit_ellipsises_and_still_measures_exactly() {
    let fitted = fit_spans(plain("abcdefghij"), 6, Style::default());
    assert_eq!(spans_width(&fitted), 6);
    assert!(fitted
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
        .ends_with('…'));
}

#[test]
fn fit_to_zero_width_produces_nothing() {
    assert!(fit_spans(plain("abc"), 0, Style::default()).is_empty());
}

#[test]
fn fit_keeps_the_last_surviving_style_on_the_ellipsis() {
    // A truncated coloured row must not end in an uncoloured character.
    let red = Style::default().fg(Color::Red);
    let spans = vec![Span::styled("abcde".to_string(), red)];
    let fitted = fit_spans(spans, 3, Style::default());
    let ellipsis = fitted
        .iter()
        .find(|s| s.content.as_ref() == "…")
        .expect("ellipsis");
    assert_eq!(ellipsis.style.fg, Some(Color::Red));
}

#[test]
fn fit_with_a_wide_glyph_at_the_boundary_still_fits() {
    // The wide glyph cannot be halved, so the result is one column short of the
    // limit before padding brings it back up.
    let fitted = fit_spans(plain("日本語です"), 5, Style::default());
    assert_eq!(spans_width(&fitted), 5);
}

#[test]
fn drop_leading_splits_inside_a_span() {
    let spans = vec![
        Span::raw("abc".to_string()),
        Span::styled("defg".to_string(), Style::default().fg(Color::Blue)),
    ];
    let kept = drop_leading(spans, 4);
    assert_eq!(
        kept.iter().map(|s| s.content.as_ref()).collect::<String>(),
        "efg"
    );
    assert_eq!(kept[0].style.fg, Some(Color::Blue));
}

#[test]
fn wrap_breaks_on_the_last_space() {
    let lines = wrap_spans(&plain("hello world again"), 11);
    let rendered: Vec<String> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(rendered, vec!["hello world", "again"]);
}

#[test]
fn wrap_hard_breaks_a_word_with_no_space_in_it() {
    let lines = wrap_spans(&plain("abcdefghij"), 4);
    let rendered: Vec<String> = lines
        .iter()
        .map(|l| l.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(rendered, vec!["abcd", "efgh", "ij"]);
}

#[test]
fn wrap_honours_embedded_newlines() {
    let lines = wrap_spans(&plain("a\nb"), 20);
    assert_eq!(lines.len(), 2);
}

#[test]
fn wrap_below_two_columns_gives_up_rather_than_looping() {
    let lines = wrap_spans(&plain("abc"), 1);
    assert_eq!(lines.len(), 1);
}

#[test]
fn wrap_counts_a_zero_width_character_as_one_slot() {
    // A run of combining marks has zero display width; counting it as zero means
    // the line never triggers a break and runs off the pane.
    let combining = "a\u{0301}".repeat(10);
    let lines = wrap_spans(&plain(&combining), 4);
    assert!(lines.len() > 1, "zero-width run never wrapped");
}

#[test]
fn split_respects_the_configured_percentage() {
    let layout = split(Rect::new(0, 0, 100, 30), 25, false);
    assert_eq!(layout.detail.width, 25);
    assert_eq!(layout.list.width, 75);
    assert_eq!(layout.header.height, HEADER_H);
    assert_eq!(layout.status.height, 1);
    assert_eq!(layout.body.height, 30 - 4);
}

#[test]
fn split_swaps_the_panes_when_asked() {
    let normal = split(Rect::new(0, 0, 100, 30), 25, false);
    let swapped = split(Rect::new(0, 0, 100, 30), 25, true);
    assert_eq!(normal.list.x, 0);
    assert_eq!(swapped.detail.x, 0);
    assert_eq!(swapped.list.x, 25);
}

#[test]
fn split_never_collapses_a_pane_on_a_narrow_terminal() {
    let layout = split(Rect::new(0, 0, 40, 20), 80, false);
    assert!(layout.detail.width >= MIN_PANE_COLS);
    assert!(layout.list.width >= MIN_PANE_COLS);
}

#[test]
fn keep_visible_scrolls_only_as_far_as_it_must() {
    assert_eq!(keep_visible(0, 5, 10, 40), 0, "selection above the window");
    assert_eq!(keep_visible(12, 0, 10, 40), 3, "selection below the window");
    assert_eq!(
        keep_visible(5, 2, 10, 40),
        2,
        "already visible, do not move"
    );
    assert_eq!(keep_visible(39, 0, 10, 40), 30, "clamped to the last page");
}

#[test]
fn header_centres_the_title() {
    let buffer = render(60, 3, |frame| {
        render_header(
            frame,
            frame.area(),
            "Dashboard",
            Stats {
                total_sessions: 3,
                total_projects: 2,
            },
            None,
            None,
        );
    });
    let rendered = text(&buffer);
    let title_row = rendered.lines().nth(1).expect("title row");
    assert!(title_row.contains("Dashboard"), "{title_row}");
    assert!(title_row.contains("3s / 2 proj"), "{title_row}");
    let offset = title_row.find("Dashboard").expect("title");
    assert!(offset > 10, "title was not centred: {title_row}");
}

#[test]
fn header_usage_slot_is_empty_until_a_readout_exists() {
    let without = text(&render(60, 3, |frame| {
        render_header(frame, frame.area(), "D", Stats::default(), None, None);
    }));
    let readout = UsageReadout {
        session_pct: Some(42),
        week_pct: Some(7),
        fable_pct: None,
    };
    let with = text(&render(60, 3, |frame| {
        render_header(
            frame,
            frame.area(),
            "D",
            Stats::default(),
            Some(&readout),
            None,
        );
    }));
    assert!(!without.contains("42%"));
    assert!(with.contains("42%"), "{with}");
}

#[test]
fn header_usage_never_shifts_the_title() {
    // The readout is overlaid on the left, not prepended — so the title's column
    // is identical with and without it.
    let readout = UsageReadout {
        session_pct: Some(42),
        week_pct: Some(7),
        fable_pct: Some(3),
    };
    let title_column = |usage: Option<&UsageReadout>| {
        let buffer = render(90, 3, |frame| {
            render_header(
                frame,
                frame.area(),
                "Dashboard",
                Stats::default(),
                usage,
                None,
            );
        });
        text(&buffer)
            .lines()
            .nth(1)
            .and_then(|row| row.find("Dashboard"))
            .expect("title")
    };
    assert_eq!(title_column(None), title_column(Some(&readout)));
}

#[test]
fn usage_readout_falls_back_to_bare_percentages_when_cramped() {
    let readout = UsageReadout {
        session_pct: Some(42),
        week_pct: Some(7),
        fable_pct: None,
    };
    assert_eq!(
        readout.format(80).as_deref(),
        Some(" session 42%  week 7% ")
    );
    assert_eq!(readout.format(12).as_deref(), Some(" s42% w7% "));
    assert_eq!(readout.format(4), None, "no form fits, so nothing is drawn");
    assert_eq!(UsageReadout::default().format(80), None);
}
