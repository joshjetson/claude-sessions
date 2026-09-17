//! How a board row reads: the status glyph, the badges after the name, and the
//! deadline at the end.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local, NaiveDate, TimeZone};

use super::row::{Role, Row, RowBuilder, Style};
use super::BoardItem;
use crate::types::{Color, NotificationLevel, NotificationStatus, Task};
use crate::util::{time_ago, truncate};

/// What a session working a task is doing. The daemon owns the full record;
/// the row only needs this much of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSessionStatus {
    Running,
    Done,
}

/// An auto-dev-daemon state, as the tag reader resolves it. A function slot
/// rather than a dependency: the board draws the marker, the auto-dev module
/// decides what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMarker {
    pub marker: String,
    pub color: Color,
}

/// Resolves a task's auto-dev state from its Odoo tags.
pub type AutoDevResolver = fn(&[String]) -> Option<AutoMarker>;

/// Everything the daemon knows that changes how a task row reads.
///
/// Every collection is borrowed: the daemon owns one of each and the board is
/// re-rendered constantly, so a row must never cost a map copy. `None` means
/// "nothing known", which is also what [`BoardCtx::default`] gives you.
#[derive(Default)]
pub struct BoardCtx<'a> {
    /// Sessions the dashboard launched, by task id.
    pub task_sessions: Option<&'a HashMap<i64, TaskSessionStatus>>,
    /// Tasks whose completion marker has landed but whose board row has not
    /// caught up yet.
    pub done_tasks: Option<&'a HashSet<i64>>,
    pub archived_tasks: Option<&'a HashSet<i64>>,
    /// Tasks the readiness gate stopped with questions waiting for a human.
    pub blocked_tasks: Option<&'a HashSet<i64>>,
    /// Recorded Optics processes per task.
    pub optics_tasks: Option<&'a HashMap<i64, usize>>,
    /// Tasks a live transcript says are being worked right now.
    ///
    /// The fallback that makes the running marker right after a daemon restart:
    /// a session started outside the dashboard, or before the restart, is still
    /// working the task even though `task_sessions` is empty.
    pub live_task_ids: Option<&'a HashSet<i64>>,
    /// The blink tick — unread notifications flash on it.
    pub blink_on: bool,
    /// Resolves a task's auto-dev state from its tags. A slot rather than a
    /// dependency: the board draws the marker, the auto-dev module decides what
    /// it is.
    pub auto_dev: Option<AutoDevResolver>,
    /// Passed in rather than read here, so one render sees one instant.
    pub now: Option<DateTime<Local>>,
    /// How wide the list pane is. A QA run's status column drops to its glyph
    /// form below [`crate::qarun::QA_WIDE_MIN_COLS`]; every other row ignores
    /// this. Zero means "unknown", which reads as narrow.
    pub tree_cols: u16,
}

impl BoardCtx<'_> {
    fn now(&self) -> DateTime<Local> {
        self.now.unwrap_or_else(Local::now)
    }

    fn has(set: Option<&HashSet<i64>>, task_id: i64) -> bool {
        set.is_some_and(|set| set.contains(&task_id))
    }
}

/// The per-task display bits shared by task and subtask rows.
struct TaskMarkers {
    marker: Row,
    id_tag: Row,
    story_points: Row,
    archived: Row,
    auto: Row,
    optics: Row,
    blocked: Row,
}

fn task_markers(task: &Task, ctx: &BoardCtx<'_>) -> TaskMarkers {
    let needs_info = BoardCtx::has(ctx.blocked_tasks, task.id);
    let session = ctx
        .task_sessions
        .and_then(|sessions| sessions.get(&task.id))
        .copied();
    let mut marker = RowBuilder::new();
    if BoardCtx::has(ctx.done_tasks, task.id) {
        marker.styled("✓", Role::Ok);
    } else if needs_info {
        marker.styled("🚧", Role::Warn);
    } else if task.open_blocker_count > 0 {
        // Odoo says another task has to land first — distinct from the
        // readiness gate's "needs info" above.
        marker.styled("⛔", Role::Danger);
    } else if let Some(status) = session {
        marker.styled(
            match status {
                TaskSessionStatus::Done => "✓",
                TaskSessionStatus::Running => "⟳",
            },
            Role::Ok,
        );
    } else if BoardCtx::has(ctx.live_task_ids, task.id) {
        marker.styled("⟳", Role::Ok);
    } else {
        marker.styled("○", Role::Dim);
    }

    let auto = ctx
        .auto_dev
        .and_then(|resolve| resolve(&task.tags))
        .map(|state| {
            let mut row = RowBuilder::new();
            row.plain(" ")
                .styled(state.marker, Role::from_color(state.color));
            row.build()
        })
        .unwrap_or_default();

    let optics_count = ctx
        .optics_tasks
        .and_then(|optics| optics.get(&task.id))
        .copied()
        .unwrap_or(0);

    TaskMarkers {
        marker: marker.build(),
        id_tag: one(format!("#{}", task.id), Role::Id),
        story_points: task
            .story_points
            .map(|points| prefixed(format!("{points}sp"), Role::Warn))
            .unwrap_or_default(),
        archived: if BoardCtx::has(ctx.archived_tasks, task.id) {
            prefixed("💾", Role::Accent)
        } else {
            Row::new()
        },
        auto,
        optics: if optics_count > 0 {
            prefixed(format!("🔬{optics_count}"), Role::Info)
        } else {
            Row::new()
        },
        blocked: if needs_info {
            prefixed("needs-info", Role::Warn)
        } else {
            Row::new()
        },
    }
}

/// A due date: red once it has passed, because that is the only state anybody
/// needs to spot from across the room. Shared with the deploy rows.
pub(super) fn deadline_row(deadline: Option<&str>, now: DateTime<Local>) -> Row {
    let Some(deadline) = deadline.filter(|value| !value.is_empty()) else {
        return Row::new();
    };
    // MM-DD — the year is noise on a board you look at every day.
    let label = deadline.get(5..).unwrap_or(deadline);
    let due = NaiveDate::parse_from_str(deadline, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .and_then(|midnight| now.timezone().from_local_datetime(&midnight).single());
    match due {
        Some(due) if due < now => prefixed(format!("⚠ {label}"), Role::Danger),
        _ => prefixed(format!("⏱ {label}"), Role::Dim),
    }
}

pub fn format_board_item(item: &BoardItem<'_>, ctx: &BoardCtx<'_>) -> Row {
    let mut row = RowBuilder::new();
    match item {
        BoardItem::Separator => {}

        BoardItem::Info { name } => {
            row.plain("  ").styled(*name, Role::Dim);
        }

        BoardItem::NotificationHeader { unread, total } => {
            let text = "🔔 Notifications";
            if *unread == 0 {
                row.push(text, Style::new(Role::Dim).bold());
            } else if ctx.blink_on {
                // Flash while unread: inverted on the tick, coloured off it.
                row.push(format!(" {text} "), Style::new(Role::Warn).invert());
            } else {
                row.push(text, Style::new(Role::Warn).bold());
            }
            row.plain("  ")
                .styled(format!("({unread} unread / {total})"), Role::Dim);
        }

        BoardItem::Notification { notif } => {
            let (role, dot) = match notif.level {
                NotificationLevel::Success => (Role::Ok, "✓"),
                _ => (Role::Warn, "●"),
            };
            let title = truncate(&notif.title, 34);
            row.plain("   ");
            if notif.status != NotificationStatus::Unread {
                // Read rows stay dim — no flash.
                row.styled(format!("{dot} "), Role::Dim)
                    .push(title, Style::new(Role::Dim).bold());
            } else if ctx.blink_on {
                row.push(format!(" {dot} {title} "), Style::new(role).invert());
            } else {
                row.styled(format!("{dot} "), role)
                    .push(title, Style::new(role).bold());
            }
            row.plain("  ");
            if !notif.project.is_empty() {
                row.styled(truncate(&notif.project, 16), Role::Project)
                    .plain("  ");
            }
            if !notif.message.is_empty() {
                row.styled(truncate(&notif.message, 40), Role::Dim);
            }
            row.plain("  ").styled(
                crate::util::parse_timestamp(&notif.ts)
                    .map(|ts| time_ago(ts, ctx.now().with_timezone(&chrono::Utc)))
                    .unwrap_or_default(),
                Role::Dim,
            );
        }

        BoardItem::Project {
            name,
            task_count,
            expanded,
            ..
        } => {
            let tasks = if *task_count == 1 {
                "1 task".to_string()
            } else {
                format!("{task_count} tasks")
            };
            row.plain(arrow(*expanded))
                .plain(" 📋 ")
                .push(*name, Style::plain().bold())
                .plain("  ")
                .styled(tasks, Role::Dim);
        }

        BoardItem::Stage {
            stage_name,
            count,
            expanded,
            ..
        } => {
            row.plain(format!("  {} ", arrow(*expanded)))
                .styled(*stage_name, Role::Accent)
                .plain("  ")
                .styled(format!("({count})"), Role::Dim);
        }

        BoardItem::Task {
            task,
            sub_count,
            sub_expanded,
            ..
        } => {
            let markers = task_markers(task, ctx);
            row.plain("    ");
            if *sub_count > 0 {
                row.styled(format!("{} ", arrow(*sub_expanded)), Role::Accent);
            } else {
                row.plain("  ");
            }
            row.extend(markers.marker).plain(" ");
            priority(&mut row, task);
            row.plain(truncate(&task.name, 42))
                .plain("  ")
                .extend(markers.id_tag)
                .extend(markers.story_points)
                .extend(markers.archived)
                .extend(markers.auto)
                .extend(markers.optics)
                .extend(markers.blocked);
            if *sub_count > 0 {
                row.extend(prefixed(format!("⊕{sub_count}"), Role::Accent));
            }
            row.extend(deadline_row(task.deadline.as_deref(), ctx.now()));
        }

        BoardItem::QaRun {
            run,
            summary,
            expanded,
        } => {
            let wide = crate::qarun::is_wide(ctx.tree_cols);
            row.plain("    ")
                .styled(format!("{} ", arrow(*expanded)), Role::Accent);
            let text = crate::qarun::run_header_text(run, summary, wide);
            // The header flashes only while something waits on the reviewer,
            // and stops the moment nothing does. A header that always blinks is
            // a header nobody reads.
            if summary.wants_attention() {
                if ctx.blink_on {
                    row.push(format!(" ⠿ {text} "), Style::new(Role::Warn).invert());
                } else {
                    row.push(format!("⠿ {text}"), Style::new(Role::Warn).bold());
                }
            } else if summary.finished() {
                row.styled("✓ ", Role::Ok)
                    .push(text, Style::new(Role::Plain).bold());
            } else {
                row.styled("⠿ ", Role::Accent)
                    .push(text, Style::new(Role::Plain).bold());
            }
        }

        BoardItem::QaRunTask { entry, task, .. } => {
            let wide = crate::qarun::is_wide(ctx.tree_cols);

            // Deliberately leaner than an ordinary task row, in two ways.
            //
            // No status marker: an ordinary row leads with ○ / ⟳ / ✓, which
            // answers "is anything happening here". Inside a run the QA cell
            // answers that better, and carrying both produced rows reading
            // "○ … ✓ PASS" — two markers disagreeing about one task.
            //
            // No priority, story points, archive or auto-dev markers: those
            // answer "should I pick this up", which the run answered by picking
            // it up. What that buys is width, and the width goes to the name.
            const INDENT: u16 = 8;
            const ID_COL: u16 = 8;
            let cell_col: u16 = if wide { 15 } else { 2 };
            let name_width =
                ctx.tree_cols
                    .saturating_sub(INDENT + ID_COL + cell_col + 2)
                    .max(16) as usize;

            let name = match task {
                Some(task) => truncate(&task.name, name_width),
                // The task left the stage but the run still owns it.
                None => format!("#{}", entry.task_id),
            };
            let id_text = format!("#{}", entry.task_id);

            row.plain(" ".repeat(INDENT as usize))
                .plain(pad(&name, name_width + 1))
                .styled(id_text.clone(), Role::Id)
                .plain(pad(&id_text, ID_COL as usize));

            let cell = crate::qarun::qa_cell_text(entry, wide);
            row.styled(cell, status_role(entry.status));
        }

        BoardItem::Subtask { task, .. } => {
            let markers = task_markers(task, ctx);
            row.plain("         ")
                .styled("↳", Role::Dim)
                .plain(" ")
                .extend(markers.marker)
                .plain(" ");
            priority(&mut row, task);
            row.plain(truncate(&task.name, 38))
                .plain("  ")
                .extend(markers.id_tag)
                .extend(markers.story_points)
                .extend(markers.archived)
                .extend(markers.auto)
                .extend(markers.optics)
                .extend(markers.blocked);
            if !task.stage_name.is_empty() {
                row.plain("  ")
                    .styled(truncate(&task.stage_name, 18), Role::Dim);
            }
            row.extend(deadline_row(task.deadline.as_deref(), ctx.now()));
        }
    }
    row.build()
}

fn arrow(expanded: bool) -> &'static str {
    if expanded {
        "▼"
    } else {
        "▶"
    }
}

/// Odoo priority is a string: anything but "0" is starred.
fn priority(row: &mut RowBuilder, task: &Task) {
    if task.priority.as_deref().is_some_and(|value| value != "0") {
        row.styled("★", Role::Warn).plain(" ");
    }
}

fn one(text: impl Into<String>, role: Role) -> Row {
    let mut row = RowBuilder::new();
    row.styled(text, role);
    row.build()
}

/// A badge with the space that separates it from what came before.
fn prefixed(text: impl Into<String>, role: Role) -> Row {
    let mut row = RowBuilder::new();
    row.plain(" ").styled(text, role);
    row.build()
}


/// Pad to a fixed column on the DISPLAY width, never the byte length: the
/// glyphs in these rows are multi-byte, and counting bytes pushes every column
/// out by their length.
fn pad(text: &str, width: usize) -> String {
    let used = unicode_width::UnicodeWidthStr::width(text);
    " ".repeat(width.saturating_sub(used).max(1))
}

/// The colour a run status reads in.
fn status_role(status: crate::qarun::QaStatus) -> Role {
    use crate::qarun::QaStatus;
    match status {
        QaStatus::Asks => Role::Warn,
        QaStatus::Stalled | QaStatus::Revisions => Role::Danger,
        QaStatus::Pass => Role::Ok,
        QaStatus::Testing => Role::Accent,
        QaStatus::Queued => Role::Dim,
    }
}
