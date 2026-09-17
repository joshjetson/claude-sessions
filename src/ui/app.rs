//! One frame: state in, a painted buffer out.
//!
//! Ported from the render half of the Node app's `src/tui/App.js`. The rule that
//! shapes this file is brief §10 mandate #7 — format only what is about to be
//! drawn. Node formatted every row of every list on every frame and then threw
//! away the ones that did not fit; here the window is computed first and the
//! formatter is called `content_height` times.

use chrono::Local;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::ui::components::{
    keep_visible, render_content, render_header, render_list, render_status, split,
};
use crate::ui::conversation::build_conversation_lines;
use crate::ui::spans::wrap_line;
use crate::ui::state::{AppState, Pane, View};
use crate::ui::tree::{build_grouped_tree, format_tree_item, TreeItem};

pub const TITLE: &str = "Claude Sessions Dashboard";

/// Draw the whole screen.
pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    let chat = state.config.chat();
    let layout = split(area, chat.conversation_width, chat.swap_panels);

    render_header(
        frame,
        layout.header,
        TITLE,
        state.stats,
        state.usage.as_ref(),
        None,
        pending_asks(state),
    );
    draw_list(frame, state, layout.list);
    draw_detail(frame, state, layout.detail);
    draw_status(frame, state, layout.status);

    // The dialog is drawn last and over the body, so it is genuinely modal
    // rather than merely on top of one pane.
    if let Some(mut dialog) = state.dialog.take() {
        dialog.render(frame, layout.body, &state.config);
        state.dialog = Some(dialog);
    }
}

/// Where a dialog gets to draw and page itself. Exposed so key handling and
/// drawing agree about the viewer's page height.
pub fn body_area(area: Rect, state: &AppState) -> Rect {
    let chat = state.config.chat();
    split(area, chat.conversation_width, chat.swap_panels).body
}

fn draw_list(frame: &mut Frame, state: &mut AppState, area: Rect) {
    match state.view {
        View::Sessions => draw_sessions(frame, state, area),
        View::Board => draw_board(frame, state, area),
        View::Deploy => draw_deploy(frame, state, area),
    }
}

fn placeholder_lines(text: &[&str]) -> Vec<Line<'static>> {
    text.iter()
        .map(|line| {
            Line::from(ratatui::text::Span::styled(
                (*line).to_string(),
                ratatui::style::Style::default().fg(crate::ui::theme::color_from_name("gray")),
            ))
        })
        .collect()
}

fn draw_sessions(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // The block's inner height is the content budget; borders are two rows.
    let content_h = area.height.saturating_sub(2) as usize;
    let groups = state.config.groups();
    let items: Vec<TreeItem<'_>> = build_grouped_tree(
        &state.by_project,
        &state.expanded_projects,
        &groups,
        &state.discovered_dirs,
    );

    if items.is_empty() {
        // An empty list that will never fill is indistinguishable from a broken
        // one unless it says which it is, so a platform that cannot read the
        // process table says so here — where somebody is already looking for
        // the sessions that are not appearing.
        let mut lines = placeholder_lines(&["No active sessions found."]);
        let width = area.width.saturating_sub(2).max(1) as usize;
        // Everything that can explain the emptiness, in the one place somebody
        // is already looking for the sessions that are not appearing: the
        // platform's own limits, then the feed's, then the transcript store's.
        for notice in crate::platform::discovery_notice()
            .map(str::to_string)
            .into_iter()
            .chain(state.feed_notice.clone())
            .chain(state.transcripts_notice.clone())
        {
            lines.push(Line::raw(""));
            lines.extend(
                wrap_line(&placeholder_lines(&[&notice])[0], width)
                    .into_iter()
                    .map(Line::from),
            );
        }
        render_list(
            frame,
            area,
            " Sessions ",
            lines,
            None,
            state.focus == Pane::Tree,
        );
        return;
    }

    let keys: Vec<String> = items.iter().map(TreeItem::key).collect();
    let selected = state.tree_sel.resolve(&keys);
    let top = keep_visible(selected, state.list_scroll, content_h, items.len());
    state.list_scroll = top;

    // ONLY the window is formatted.
    let rows: Vec<Line<'static>> = items
        .iter()
        .skip(top)
        .take(content_h)
        .map(|item| format_tree_item(item, &state.config))
        .collect();

    render_list(
        frame,
        area,
        " Sessions ",
        rows,
        Some(selected.saturating_sub(top)),
        state.focus == Pane::Tree,
    );
}

/// The board tab's list. Only the window is formatted (brief §10 mandate #7);
/// the blink phase is derived from the clock here rather than kept as state the
/// loop has to remember to toggle.
fn draw_board(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let content_h = area.height.saturating_sub(2) as usize;
    state.board.blink_on = Local::now().timestamp() % 2 == 0;
    let label = crate::ui::board::label(&state.board);

    // One walk of the row list: the window places itself from the scroll
    // position it is handed, and only the visible rows are formatted.
    // The pane's inner width, less its border. A QA run's status column drops
    // to its glyph form on a narrow pane and nothing else reads it.
    let view = crate::ui::board::window(
        state,
        state.list_scroll,
        content_h,
        area.width.saturating_sub(2),
    );
    state.list_scroll = view.top;
    render_list(
        frame,
        area,
        &label,
        view.lines,
        Some(view.selected.saturating_sub(view.top)),
        state.focus == Pane::Tree,
    );
}

/// The Deploy tab's list. Only the window is formatted, like the board's.
fn draw_deploy(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let content_h = area.height.saturating_sub(2) as usize;
    let label = crate::ui::deploy::label(&state.deploy);
    let view = crate::ui::deploy::window(state, state.list_scroll, content_h);
    state.list_scroll = view.top;
    render_list(
        frame,
        area,
        &label,
        view.lines,
        Some(view.selected.saturating_sub(view.top)),
        state.focus == Pane::Tree,
    );
}

fn draw_detail(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let content_h = area.height.saturating_sub(2) as usize;
    let focused = state.focus == Pane::Conversation;

    // Owned rather than borrowed: the board's pane carries its own label (a
    // task or a notification), and the scroll fields below are written to the
    // same `state` the label would otherwise be borrowed out of.
    let (label, lines): (String, Vec<Line<'static>>) = match (&state.flash, state.view) {
        (Some(flash), view) => (
            match view {
                View::Board => " Task ".to_string(),
                View::Deploy => " Deploy ".to_string(),
                View::Sessions => " Conversation ".to_string(),
            },
            flash
                .split('\n')
                .map(|l| Line::raw(l.to_string()))
                .collect(),
        ),
        (None, View::Sessions) => (
            " Conversation ".to_string(),
            build_conversation_lines(
                &state.conv.messages,
                state.conv.meta.as_ref(),
                state.config.chat(),
            ),
        ),
        (None, View::Board) => match &state.board.detail {
            Some(detail) => (
                detail.label.clone(),
                detail
                    .rows
                    .iter()
                    .map(|row| crate::ui::spans::row_line(row))
                    .collect(),
            ),
            None => (
                " Task ".to_string(),
                placeholder_lines(&[
                    "Select a task (→) to preview, Enter for its action menu.",
                    "",
                    "s start   v revise   C resume chat   P pipeline   m stage",
                    "R watch a stage as a QA run   ] jump to the next agent waiting",
                    "g session   G terminal   o browser   S ssh",
                    "f mine/all   p projects   x dismiss a notification",
                ]),
            ),
        },
        (None, View::Deploy) => match &state.deploy.detail {
            Some(detail) => (
                detail.label.clone(),
                detail
                    .rows
                    .iter()
                    .map(|row| crate::ui::spans::row_line(row))
                    .collect(),
            ),
            None => (
                " Deploy ".to_string(),
                placeholder_lines(&[
                    "Press r to load the deploy board — it never refreshes on its own,",
                    "because every refresh costs a GitLab call per open merge request.",
                    "",
                    "→ preview   Enter menu   m merge   M merge all ready",
                    "R resolve conflicts   d deploy   X cancel   L output",
                    "g session   G terminal   o open MR   t open task   c configure",
                ]),
            ),
        },
    };

    let wrapped: Vec<Vec<ratatui::text::Span<'static>>> = lines
        .iter()
        .flat_map(|line| wrap_line(line, inner_w.max(1)))
        .collect();

    state.conv.total_lines = wrapped.len();
    state.conv.page_height = content_h;
    let max = wrapped.len().saturating_sub(content_h);
    if state.conv.stick {
        state.conv.scroll_top = max;
    }
    let top = state.conv.scroll_top.min(max);
    state.conv.scroll_top = top;

    let window: Vec<Line<'static>> = wrapped
        .into_iter()
        .skip(top)
        .take(content_h)
        .map(Line::from)
        .collect();
    render_content(frame, area, &label, window, focused);
}

fn draw_status(frame: &mut Frame, state: &AppState, area: Rect) {
    let clock = Local::now().format("%I:%M:%S %p").to_string();
    let feed = state.feed.label(std::time::Instant::now());
    render_status(frame, area, &clock, &feed, &status_hints(state));
}

/// The contextual key hints. Each view advertises only what it can actually do
/// right now — a hint for a key that does nothing is worse than no hint.
pub fn status_hints(state: &AppState) -> Vec<(&'static str, &'static str)> {
    let mut hints: Vec<(&'static str, &'static str)> = vec![("Tab", "view")];
    match state.view {
        View::Sessions => {
            hints.extend([
                ("S-Tab", "panel"),
                ("←→", "expand"),
                ("Enter", "select"),
                ("o", "terminal"),
                ("x", "kill"),
                ("r", "rename"),
                ("n", "new"),
                ("a/d", "group"),
            ]);
            if state.focus == Pane::Conversation {
                hints.extend([
                    ("t", "ts"),
                    ("f", "filter"),
                    ("/", "search"),
                    ("g/G", "top/end"),
                ]);
            }
            hints.push(("s", "settings"));
        }
        View::Board => hints.extend([
            ("←→", "expand"),
            ("Enter", "menu"),
            ("s", "start"),
            ("R", "QA run"),
            ("]", "next ask"),
            ("v", "revise"),
            ("C", "chat"),
            ("P", "pipeline"),
            ("m", "stage"),
            ("g/G", "session"),
            ("o", "browser"),
            ("S", "ssh"),
            ("f", "filter"),
            ("p", "projects"),
            ("r", "refresh"),
        ]),
        View::Deploy => hints.extend([
            ("←→", "expand"),
            ("Enter", "menu"),
            ("m/M", "merge"),
            ("R", "conflicts"),
            ("d", "deploy"),
            ("X", "cancel"),
            ("L", "output"),
            ("g/G", "session"),
            ("o", "MR"),
            ("t", "task"),
            ("c", "config"),
            ("r", "refresh"),
        ]),
    }
    hints.extend([("q", "quit"), ("Q", "stop all")]);
    hints
}


/// How many QA agents are waiting on a decision, across every run.
///
/// Counted per frame, which it can afford to be: it reads one JSON file per
/// task in a run and nothing else — no git, no Odoo. Zero when nothing is being
/// watched, which is the common case and costs nothing at all.
fn pending_asks(state: &AppState) -> usize {
    if state.board.runs.is_empty() {
        return 0;
    }
    let sessions = std::collections::HashMap::new();
    let ctx = state.board.run_ctx(
        &state.paths,
        &state.notifications,
        &sessions,
        std::time::SystemTime::now(),
    );
    crate::qarun::pending_asks(&state.board.runs, &ctx).len()
}
