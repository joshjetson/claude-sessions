//! Layout and the three chrome pieces: header band, bordered panes, status bar.
//!
//! Ported from the Node app's `src/tui/components.js`. The windowing arithmetic
//! that used to live there is in [`crate::ui::spans`]; what remains here is the
//! geometry and the drawing, both of which are pure functions of the state plus
//! the rectangle they were handed.

use ratatui::layout::Rect;
use ratatui::style::{Color as TuiColor, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::Frame;

use crate::ui::spans::{centered, drop_leading, fit_spans, spans_width};
use crate::ui::state::{Stats, UsageReadout};
use crate::ui::theme::color_from_name;

pub const HEADER_H: u16 = 3;
pub const STATUS_H: u16 = 1;
/// Neither pane is ever allowed to collapse: a one-column conversation pane is
/// not a smaller pane, it is a broken frame.
pub const MIN_PANE_COLS: u16 = 12;
/// The status bar's background — the same `#333` the Node app used, kept as a
/// specific shade because it has to read as "chrome" against any palette.
const STATUS_BG: TuiColor = TuiColor::Rgb(0x33, 0x33, 0x33);

/// Where the four regions of the screen are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub header: Rect,
    pub body: Rect,
    pub status: Rect,
    /// The tree/list pane, wherever `swapPanels` put it.
    pub list: Rect,
    pub detail: Rect,
}

/// Split the screen. `conversation_pct` is clamped the way the settings grid
/// clamps it, so a hand-edited config cannot produce a pane with no room in it.
pub fn split(area: Rect, conversation_pct: u16, swap: bool) -> Layout {
    let header = Rect {
        height: HEADER_H.min(area.height),
        ..area
    };
    let status_h = STATUS_H.min(area.height.saturating_sub(header.height));
    let body_h = area.height.saturating_sub(header.height + status_h);
    let body = Rect {
        y: area.y + header.height,
        height: body_h,
        ..area
    };
    let status = Rect {
        y: body.y + body.height,
        height: status_h,
        ..area
    };

    let pct = conversation_pct.clamp(10, 80) as u32;
    // Rounded, not ceilinged: Node's `Math.round(cols * pct / 100)`
    // (`App.js:166`), so the same percentage gives the same column on both.
    let mut detail_w = ((area.width as u32 * pct + 50) / 100) as u16;
    // Floor then ceiling, in that order and never as a `clamp` — below about 24
    // columns the floor is above the ceiling, and `clamp` panics when it is.
    // Node's paired `Math.max`/`Math.min` simply degraded, and a dashboard that
    // panics because a window got dragged narrow is worse than a cramped one.
    detail_w = detail_w
        .max(MIN_PANE_COLS)
        .min(area.width.saturating_sub(MIN_PANE_COLS))
        .max(1)
        .min(area.width);
    let list_w = area.width.saturating_sub(detail_w);

    let (list_x, detail_x) = if swap {
        (body.x + detail_w, body.x)
    } else {
        (body.x, body.x + list_w)
    };
    Layout {
        header,
        body,
        status,
        list: Rect {
            x: list_x,
            y: body.y,
            width: list_w,
            height: body.height,
        },
        detail: Rect {
            x: detail_x,
            y: body.y,
            width: detail_w,
            height: body.height,
        },
    }
}

/// Scroll the window just far enough to keep `selected` inside it.
pub fn keep_visible(selected: usize, scroll_top: usize, content_h: usize, total: usize) -> usize {
    if content_h == 0 {
        return 0;
    }
    let mut top = scroll_top;
    if selected < top {
        top = selected;
    }
    if selected >= top + content_h {
        top = selected + 1 - content_h;
    }
    top.min(total.saturating_sub(content_h))
}

/// The header band: three rows of blue, the title centred on the full width,
/// and the usage readout overlaid on the LEFT.
///
/// Overlaid rather than prepended on purpose — the title stays centred on the
/// terminal, so the readout appearing (or growing) never shifts it sideways.
pub fn render_header(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    stats: Stats,
    usage: Option<&UsageReadout>,
    alert: Option<Alert>,
    asks: usize,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let band = Style::default().bg(color_from_name("blue"));
    let cols = area.width as usize;

    let mut title_spans = vec![
        Span::styled(format!(" {title} "), band.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(
                "  —  {}s / {} proj",
                stats.total_sessions, stats.total_projects
            ),
            band,
        ),
    ];
    if let Some(alert) = alert {
        title_spans.push(Span::styled("   ", band));
        title_spans.push(alert.span());
    }
    // How many QA agents are waiting on a decision, across every run.
    //
    // One number, in the one place always on screen. With seven passes in
    // flight a per-row blink does not scale: blinking rows cannot be counted,
    // and the ones below the fold do not blink at all. This says whether to
    // look.
    if asks > 0 {
        let blink_on = alert.is_some_and(|alert| alert.on);
        title_spans.push(Span::styled("   ", band));
        title_spans.push(ask_span(asks, blink_on));
    }

    let title_width = spans_width(&title_spans);
    let mut line = centered(title_spans, cols, band);

    // How much room the readout has is the centred title's left edge; the
    // readout picks the widest form that fits inside it — bars first, then
    // shorter labels, then bare percentages, then nothing at all.
    let available = (cols.saturating_sub(title_width)) / 2;
    if let Some(segments) =
        usage.and_then(|u| crate::usage::format_usage(u, available.saturating_sub(1)))
    {
        let left: Vec<Span> = segments
            .iter()
            .map(|segment| {
                Span::styled(
                    segment.text.clone(),
                    band.fg(color_from_name(segment.color.as_str())),
                )
            })
            .collect();
        let left_width = spans_width(&left);
        let mut merged = left;
        merged.extend(drop_leading(line, left_width));
        line = fit_spans(merged, cols, band);
    }

    let blank = fit_spans(Vec::new(), cols, band);
    let rows = vec![
        Line::from(blank.clone()),
        Line::from(line),
        Line::from(blank),
    ];
    frame.render_widget(Paragraph::new(rows), area);
}

/// The unread-notification badge, blinking. Phase 5 feeds it; until then the
/// header simply never receives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub colour: TuiColor,
    pub unread: usize,
    pub on: bool,
}

impl Alert {
    fn span(self) -> Span<'static> {
        let label = format!(" 🔔 {} ", self.unread);
        let style = if self.on {
            Style::default()
                .bg(self.colour)
                .fg(color_from_name("black"))
        } else {
            Style::default().fg(self.colour).bg(color_from_name("blue"))
        };
        Span::styled(label, style)
    }
}

/// The "someone is waiting on you" badge, blinking on the same tick as the
/// notification one so the header has a single rhythm rather than two.
fn ask_span(asks: usize, on: bool) -> Span<'static> {
    let label = format!(
        " ⚠ {asks} {} you ",
        if asks == 1 { "agent wants" } else { "agents want" }
    );
    let style = if on {
        Style::default()
            .bg(color_from_name("yellow"))
            .fg(color_from_name("black"))
    } else {
        Style::default()
            .fg(color_from_name("yellow"))
            .bg(color_from_name("blue"))
            .add_modifier(Modifier::BOLD)
    };
    Span::styled(label, style)
}

/// A bordered pane. `label` sits on the top border, `focused` colours it.
pub fn pane_block(label: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        color_from_name("cyan")
    } else {
        color_from_name("gray")
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            label.to_string(),
            Style::default().fg(color_from_name("cyan")),
        ))
}

/// Draw a windowed list. Only `rows` — already the visible slice — is formatted;
/// `selected` is relative to that slice (brief §10 mandate #7).
pub fn render_list(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    rows: Vec<Line<'static>>,
    selected: Option<usize>,
    focused: bool,
) {
    let block = pane_block(label, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let selection_style = Style::default()
        .bg(color_from_name("blue"))
        .fg(color_from_name("white"));
    let lines: Vec<Line<'static>> = rows
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if Some(i) == selected {
                let spans = line
                    .spans
                    .into_iter()
                    .map(|s| {
                        let fg = s.style.fg.unwrap_or(color_from_name("white"));
                        Span::styled(
                            s.content,
                            selection_style.fg(fg).bg(color_from_name("blue")),
                        )
                    })
                    .collect::<Vec<_>>();
                Line::from(fit_spans(spans, width, selection_style))
            } else {
                Line::from(fit_spans(line.spans, width, Style::default()))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Draw a windowed content pane (no selection) — the conversation, or a flash.
pub fn render_content(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    rows: Vec<Line<'static>>,
    focused: bool,
) {
    let block = pane_block(label, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(rows), inner);
}

/// The one-row status bar. `hints` are drawn dim and their keys bright, which is
/// what makes a dense row of shortcuts scannable at a glance.
///
/// `feed` names where the sessions on screen came from and how stale they are.
/// It sits beside the clock rather than among the hints because it is a fact
/// about the screen, not something to press — and it goes first so that the
/// narrowest terminal still shows it while the hints are trimmed away.
pub fn render_status(
    frame: &mut Frame,
    area: Rect,
    clock: &str,
    feed: &str,
    hints: &[(&str, &str)],
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let bar = Style::default().bg(STATUS_BG);
    let dim = bar.fg(color_from_name("gray"));
    let mut spans = vec![
        Span::styled(format!(" {clock}  │"), bar),
        Span::styled(format!(" {feed} "), dim),
        Span::styled("│", bar),
    ];
    for (key, label) in hints {
        spans.push(Span::styled(format!("  {key}"), dim));
        spans.push(Span::styled(format!(" {label}"), bar));
    }
    let line = fit_spans(spans, area.width as usize, bar);
    frame.render_widget(Paragraph::new(Line::from(line)), area);
}
