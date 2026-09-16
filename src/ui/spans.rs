//! Display-width arithmetic on styled runs.
//!
//! Ported from the windowing math in the Node app's `src/tui/components.js`
//! (`clipText` / `fitSegments` / `dropLeading`) and `src/tui/markup.js`
//! (`wrapSegments`). The blessed-tag round-trip those functions existed to feed
//! is deleted — see brief §10 mandate #7 — so these operate straight on ratatui
//! [`Span`]s, and nothing in this file allocates a tag string.
//!
//! Everything here is pure and width-aware rather than length-aware: a pane
//! budget is columns, and a row of CJK sliced by character count smears the
//! border it was supposed to sit inside.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Total display width of a run of spans.
pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// The longest prefix of `text` that fits in `width` columns.
pub fn clip_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Make a run exactly `width` columns: pad with `pad` style, or clip with a
/// trailing `…`.
///
/// The ellipsis inherits the style of the last span that survived the clip (or
/// `pad` when nothing did), which is how the Node original kept a truncated
/// coloured row from ending in an uncoloured character.
pub fn fit_spans(spans: Vec<Span<'static>>, width: usize, pad: Style) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let total = spans_width(&spans);
    if total == width {
        return spans;
    }
    if total < width {
        let mut out = spans;
        out.push(Span::styled(" ".repeat(width - total), pad));
        return out;
    }

    let limit = width - 1; // one column is the ellipsis
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut last_style = pad;
    for span in spans {
        if used >= limit {
            break;
        }
        let text = clip_text(&span.content, limit - used);
        if text.is_empty() {
            continue;
        }
        used += text.width();
        last_style = span.style;
        out.push(Span::styled(text, span.style));
    }
    out.push(Span::styled("…", last_style));
    let fitted = spans_width(&out);
    if fitted < width {
        out.push(Span::styled(" ".repeat(width - fitted), pad));
    }
    out
}

/// Drop `width` columns from the start of a run, splitting a span if the cut
/// lands inside one.
///
/// This is what lets the header overlay a usage readout on the left without
/// shifting the centred title: the readout is drawn, then this many columns of
/// the centred line are thrown away.
pub fn drop_leading(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut remaining = width;
    let mut out = Vec::new();
    for span in spans {
        if remaining == 0 {
            out.push(span);
            continue;
        }
        let w = span.content.width();
        if w <= remaining {
            remaining -= w;
            continue;
        }
        let kept: String = drop_columns(&span.content, remaining);
        out.push(Span::styled(kept, span.style));
        remaining = 0;
    }
    out
}

fn drop_columns(text: &str, columns: usize) -> String {
    let mut dropped = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        if dropped >= columns {
            out.push(ch);
            continue;
        }
        dropped += ch.width().unwrap_or(0).max(1);
    }
    out
}

/// Centre a run in `cols`, filling both sides with `pad`.
pub fn centered(spans: Vec<Span<'static>>, cols: usize, pad: Style) -> Vec<Span<'static>> {
    let w = spans_width(&spans);
    let left = cols.saturating_sub(w) / 2;
    let mut out = Vec::with_capacity(spans.len() + 2);
    out.push(Span::styled(" ".repeat(left), pad));
    out.extend(spans);
    fit_spans(out, cols, pad)
}

/// Word-wrap a styled run to `width` columns, honouring embedded newlines.
///
/// Deterministic height is the whole point: every returned line fits, so the
/// conversation pane can window by line index without measuring anything twice.
/// Ported unit-for-unit from `wrapSegments`, including its two quirks — a
/// character is appended *before* the overflow check (so the break happens one
/// character late, which is what keeps a trailing wide glyph on the line it
/// belongs to), and a space at index 0 is not a break point.
pub fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    if width < 2 {
        return vec![spans.to_vec()];
    }
    struct Unit {
        ch: char,
        w: usize,
        style: Style,
    }
    let mut units: Vec<Unit> = Vec::new();
    for span in spans {
        for ch in span.content.chars() {
            units.push(Unit {
                ch,
                // A zero-width character still occupies a slot, as it did in
                // Node (`stringWidth(ch) || 1`) — otherwise a run of combining
                // marks never triggers a break and the line runs off the pane.
                w: ch.width().unwrap_or(0).max(1),
                style: span.style,
            });
        }
    }

    let mut lines: Vec<Vec<Unit>> = Vec::new();
    let mut cur: Vec<Unit> = Vec::new();
    let mut cur_w = 0usize;
    let mut last_space: Option<usize> = None;

    for unit in units {
        if unit.ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            last_space = None;
            continue;
        }
        let w = unit.w;
        let is_space = unit.ch == ' ';
        cur.push(unit);
        cur_w += w;
        if is_space {
            last_space = Some(cur.len() - 1);
        }
        if cur_w > width {
            match last_space {
                Some(at) if at > 0 => {
                    let next = cur.split_off(at + 1);
                    let head = std::mem::replace(&mut cur, next);
                    let mut head = head;
                    head.truncate(at);
                    lines.push(head);
                    cur_w = cur.iter().map(|u| u.w).sum();
                    last_space = cur.iter().rposition(|u| u.ch == ' ');
                }
                _ => {
                    let last = cur.pop().expect("just pushed");
                    lines.push(std::mem::take(&mut cur));
                    cur_w = last.w;
                    cur.push(last);
                    last_space = None;
                }
            }
        }
    }
    lines.push(cur);

    lines
        .into_iter()
        .map(|units| {
            let mut out: Vec<Span<'static>> = Vec::new();
            for unit in units {
                match out.last_mut() {
                    Some(last) if last.style == unit.style => {
                        last.content.to_mut().push(unit.ch);
                    }
                    _ => out.push(Span::styled(unit.ch.to_string(), unit.style)),
                }
            }
            out
        })
        .collect()
}

/// Wrap a whole logical line, keeping empty lines as one blank row so list
/// windowing stays exact.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Vec<Span<'static>>> {
    wrap_spans(&line.spans, width)
}
