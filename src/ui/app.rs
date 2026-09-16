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
        View::Board => render_list(
            frame,
            area,
            " Tasks Board ",
            placeholder_lines(&[
                "The Odoo task board arrives with the board phase.",
                "",
                "It brings the notification feed, the project/stage/task",
                "tree, and the task action menu.",
            ]),
            None,
            state.focus == Pane::Tree,
        ),
        View::Deploy => render_list(
            frame,
            area,
            " Deploy ",
            placeholder_lines(&[
                "The deploy board arrives with the deploy phase.",
                "",
                "It brings Deployed-stage tasks, live MR badges and the",
                "deploy runner's output pane.",
            ]),
            None,
            state.focus == Pane::Tree,
        ),
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
    let items: Vec<TreeItem<'_>> = build_grouped_tree(
        &state.by_project,
        &state.expanded_projects,
        state.config.groups(),
        &state.discovered_dirs,
    );

    if items.is_empty() {
        render_list(
            frame,
            area,
            " Sessions ",
            placeholder_lines(&["No active sessions found."]),
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

fn draw_detail(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let content_h = area.height.saturating_sub(2) as usize;
    let focused = state.focus == Pane::Conversation;

    let (label, lines): (&str, Vec<Line<'static>>) = match (&state.flash, state.view) {
        (Some(flash), view) => (
            match view {
                View::Board => " Task ",
                View::Deploy => " Deploy ",
                View::Sessions => " Conversation ",
            },
            flash
                .split('\n')
                .map(|l| Line::raw(l.to_string()))
                .collect(),
        ),
        (None, View::Sessions) => (
            " Conversation ",
            build_conversation_lines(
                &state.conv.messages,
                state.conv.meta.as_ref(),
                state.config.chat(),
            ),
        ),
        (None, View::Board) => (
            " Task ",
            placeholder_lines(&["Select a task (→) to preview.", "", "Board phase pending."]),
        ),
        (None, View::Deploy) => (" Deploy ", placeholder_lines(&["Deploy phase pending."])),
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
    render_content(frame, area, label, window, focused);
}

fn draw_status(frame: &mut Frame, state: &AppState, area: Rect) {
    let clock = Local::now().format("%I:%M:%S %p").to_string();
    render_status(frame, area, &clock, &status_hints(state));
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
        View::Board => hints.push(("r", "refresh")),
        View::Deploy => hints.push(("r", "refresh")),
    }
    hints.extend([("q", "quit"), ("Q", "stop all")]);
    hints
}
