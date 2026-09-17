//! The right-hand pane on the board tab: a task, or a notification.
//!
//! Ported from `showTaskDetail` / `showNotifDetail` in the Node app's
//! `src/tui/controller.js`. Node fetched the Odoo description inside the same
//! function that painted the pane and swapped the content twice; here the
//! header is built synchronously and the description arrives later as its own
//! update, so pressing `→` never waits on a round trip.

use crate::board::{Role, Row, RowBuilder, Style};
use crate::odoo::TaskDetail;
use crate::types::{Notification, Task};
use crate::ui::state::AppState;

use super::controller::task_sessions;
use super::slice::BoardDetail;

pub const TASK_LABEL: &str = " Task ";
pub const NOTIFICATION_LABEL: &str = " Notification ";

fn line(text: impl Into<String>, role: Role) -> Row {
    let mut row = RowBuilder::new();
    row.styled(text, role);
    row.build()
}

fn bold(text: impl Into<String>) -> Row {
    let mut row = RowBuilder::new();
    row.push(text, Style::plain().bold());
    row.build()
}

fn blank() -> Row {
    Row::new()
}

/// The part of a task's detail that needs no network: everything the board
/// already knows.
pub fn task_header(task: &Task, state: TaskState<'_>) -> Vec<Row> {
    let mut rows = vec![bold(task.name.clone())];

    let mut meta = RowBuilder::new();
    meta.styled(format!("#{}  ·  {}", task.id, task.project_name), Role::Dim);
    if let Some(points) = task.story_points {
        let plural = if points == 1 { "" } else { "s" };
        meta.plain("  ·  ")
            .styled(format!("{points} story point{plural}"), Role::Warn);
    }
    rows.push(meta.build());
    rows.push(line(task.stage_name.clone(), Role::Accent));
    if let Some(deadline) = task.deadline.as_deref().filter(|d| !d.is_empty()) {
        rows.push(line(format!("deadline: {deadline}"), Role::Dim));
    }
    if state.done {
        rows.push(line("✓ done", Role::Ok));
    } else if state.running {
        rows.push(line("⟳ session running", Role::Ok));
    }
    rows.push(blank());
    rows.push(line(
        "Enter → action menu (start / add context)   ·   s → start now",
        Role::Dim,
    ));
    if state.archived {
        rows.push(line(
            "💾 transcript archived → press v to resume for a revision, C to just talk to it",
            Role::Accent,
        ));
    }

    // The readiness gate stopped this task: show the questions it wants
    // answered, which is the whole point of the gate existing.
    if !state.questions.is_empty() {
        rows.push(blank());
        rows.push(line(
            "🚧 Needs info — the readiness gate paused this task:",
            Role::Warn,
        ));
        for question in state.questions {
            rows.push(line(format!("  • {question}"), Role::Warn));
        }
        rows.push(line(
            "Answer on the task, then press s to re-run.",
            Role::Dim,
        ));
    }

    // Recorded UI coverage, from the map the board fetch already filled. The
    // full process list is [`crate::optics::OpticsClient::task_optics`]; the
    // pane shows the count, which is the part that changes a decision.
    if let Some(count) = state.optics.filter(|count| *count > 0) {
        let plural = if count == 1 { "" } else { "es" };
        rows.push(line(
            format!("🔬 {count} recorded Optics process{plural} for this task"),
            Role::Info,
        ));
    }

    if task.open_blocker_count > 0 {
        rows.push(blank());
        rows.push(line(
            format!(
                "⛔ {} of {} blocker(s) still open — s will ask before starting.",
                task.open_blocker_count, task.blocker_count
            ),
            Role::Danger,
        ));
    }
    rows.push(blank());
    rows
}

/// What the board knows about a task beyond its Odoo record.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskState<'a> {
    pub done: bool,
    pub running: bool,
    pub archived: bool,
    pub questions: &'a [String],
    /// Recorded Optics processes, when this install has Optics configured.
    pub optics: Option<usize>,
}

/// What the board knows about a task right now.
pub fn state_of<'a>(state: &'a AppState, task_id: i64) -> TaskState<'a> {
    TaskState {
        done: state.board.done_tasks.contains(&task_id),
        running: !task_sessions(state.sessions(), task_id).is_empty()
            || state
                .board
                .link(task_id)
                .is_some_and(crate::daemon::TaskLink::is_running),
        archived: state.board.archived_tasks.contains(&task_id),
        questions: state
            .board
            .blocked_tasks
            .get(&task_id)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        optics: state.board.optics_tasks.get(&task_id).copied(),
    }
}

/// Fold a description that arrived late into the pane, if it is still showing
/// the task that asked for it. A stale answer is dropped rather than painted
/// over another row's detail.
pub fn apply_description(state: &mut AppState, task_id: i64, detail: Option<&TaskDetail>) {
    if state.board.detail.as_ref().map(|pane| pane.task_id) != Some(Some(task_id)) {
        return;
    }
    let Some(task) = state.board.task(task_id).cloned() else {
        return;
    };
    state.board.detail = Some(with_description(&task, state_of(state, task_id), detail));
    state.dirty = true;
}

/// The header plus a placeholder, shown the instant `→` is pressed.
pub fn loading(task: &Task, state: TaskState<'_>) -> BoardDetail {
    let mut rows = task_header(task, state);
    rows.push(line("loading description…", Role::Dim));
    BoardDetail {
        label: TASK_LABEL.to_string(),
        rows,
        task_id: Some(task.id),
    }
}

/// The same header with the Odoo description folded in.
pub fn with_description(
    task: &Task,
    state: TaskState<'_>,
    detail: Option<&TaskDetail>,
) -> BoardDetail {
    let mut rows = task_header(task, state);
    match detail.map(|d| html_to_text(&d.description)) {
        Some(text) if !text.is_empty() => {
            rows.extend(text.lines().map(|l| line(l.to_string(), Role::Plain)));
        }
        Some(_) => rows.push(line("(no description)", Role::Dim)),
        None => rows.push(line("(description unavailable)", Role::Dim)),
    }
    BoardDetail {
        label: TASK_LABEL.to_string(),
        rows,
        task_id: Some(task.id),
    }
}

/// A notification, and what can be done about it.
pub fn notification(notif: &Notification, has_session: bool) -> BoardDetail {
    let mut rows = vec![bold(notif.title.clone())];
    let meta = [
        notif.project.clone(),
        notif
            .task_id
            .map(|id| format!("task #{id}"))
            .unwrap_or_default(),
        notif.ts.clone(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("  ·  ");
    rows.push(line(meta, Role::Dim));
    if !notif.cwd.is_empty() {
        rows.push(line(notif.cwd.clone(), Role::Dim));
    }
    rows.push(blank());
    let message = if notif.message.is_empty() {
        "(no message)"
    } else {
        &notif.message
    };
    rows.extend(message.lines().map(|l| line(l.to_string(), Role::Plain)));
    rows.push(blank());
    rows.push(line(
        if has_session {
            "Enter menu  ·  g go to session  ·  x dismiss"
        } else {
            "Enter menu  ·  x dismiss  ·  (no live session matched)"
        },
        Role::Dim,
    ));
    BoardDetail {
        label: NOTIFICATION_LABEL.to_string(),
        rows,
        task_id: None,
    }
}

/// Odoo stores task descriptions as HTML. Flatten it to something a terminal
/// pane can show, rather than printing the tags.
///
/// Deliberately a small hand-rolled pass, not a parser: the input is Odoo's own
/// editor output, and the failure mode of a stray tag surviving is a cosmetic
/// one in a read-only pane.
pub fn html_to_text(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(html.len());
    let mut chars = html.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        let Some(end) = html[index..].find('>').map(|offset| index + offset) else {
            // An unclosed `<` is text, not markup.
            out.push(ch);
            continue;
        };
        let tag = html[index + 1..end].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split([' ', '\t', '\n', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        match (tag.starts_with('/'), name.as_str()) {
            (_, "br") => out.push('\n'),
            (true, "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") => out.push('\n'),
            (false, "li") => out.push_str("• "),
            _ => {}
        }
        while let Some((next, _)) = chars.peek() {
            if *next > end {
                break;
            }
            chars.next();
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        // Last, so an escaped entity in the source does not become a real one.
        .replace("&amp;", "&");
    collapse_blank_lines(out.trim())
}

/// Three or more newlines become two: Odoo's editor emits an empty `<p>` per
/// blank line and they stack up.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            run += 1;
            if run > 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(ch);
    }
    out
}
