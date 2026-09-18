//! The sessions tree: grouping, identity keys, and row formatting.
//!
//! Ports `buildGroupedTree` / `buildFlatTree` from the Node app's `src/state.js`
//! and every formatter from `src/tui/treefmt.js`. Rows are built as [`Line`]s on
//! demand — the caller formats only the window it is about to draw (brief §10
//! mandate #7), where Node formatted every row of every list every frame.

use std::collections::{BTreeMap, HashSet};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::{ConfigHandle, Group};
use crate::types::{Session, SessionStatus};
use crate::ui::theme::{color_from_name, color_of};
use crate::util::{child_dir_of, join_dir, trim_trailing_separators, truncate};

/// Sessions grouped by project name, which is the shape the tree is built from.
pub type SessionsByProject = BTreeMap<String, Vec<Session>>;

/// One row of the sessions tree. Borrows from the state it was built out of:
/// the tree is rebuilt every draw and never outlives the frame.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeItem<'a> {
    Separator {
        name: &'a str,
        /// `None` for the synthetic "Other sessions" divider, which belongs to
        /// no configured group and so cannot be removed with `d`.
        group_index: Option<usize>,
    },
    Project {
        name: &'a str,
        session_count: usize,
        total_tokens: u64,
        expanded: bool,
    },
    Session {
        project_name: &'a str,
        session: &'a Session,
    },
    /// A folder inside a group with nothing running in it. `n` starts a session
    /// here — it is the only way to launch into a repo that is currently quiet.
    Inactive { name: &'a str, path: String },
    /// A QA run, drawn above the projects. Collapsed it is the coordinator and
    /// nothing else, because the coordinator is the run's single point of
    /// contact. Expanded it adds the QA sessions underneath.
    Run {
        run_id: &'a str,
        stage: &'a str,
        coordinator: Option<&'a Session>,
        agents: usize,
        asking: usize,
        expanded: bool,
    },
    /// One QA session under its run. `session` is `None` for a task the run
    /// covers but has not started, which is a real state and not an error.
    RunAgent {
        run_id: &'a str,
        task_id: i64,
        session: Option<&'a Session>,
        asking: bool,
    },
}

/// A run, prepared for the tree.
///
/// Built by the caller rather than here, so this module keeps knowing only
/// about sessions and never reaches into the board.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSection<'a> {
    pub run_id: &'a str,
    pub stage: &'a str,
    pub coordinator: Option<&'a Session>,
    pub agents: Vec<RunAgentRow<'a>>,
}

/// One task of a run, prepared for the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct RunAgentRow<'a> {
    pub task_id: i64,
    pub session: Option<&'a Session>,
    pub asking: bool,
}

impl TreeItem<'_> {
    /// The identity the selection holds on to, so a reshuffle between ticks
    /// leaves the cursor on the same row rather than on the same index.
    pub fn key(&self) -> String {
        match self {
            TreeItem::Project { name, .. } => format!("p:{name}"),
            TreeItem::Session { session, .. } => format!("s:{}", session.session_id),
            TreeItem::Separator { name, .. } => format!("g:{name}"),
            TreeItem::Inactive { path, .. } => format!("i:{path}"),
            TreeItem::Run { run_id, .. } => format!("r:{run_id}"),
            TreeItem::RunAgent {
                run_id, task_id, ..
            } => format!("ra:{run_id}:{task_id}"),
        }
    }

    /// The directory `n` would launch a new session in, if any.
    pub fn dir_path(&self, by_project: &SessionsByProject) -> Option<String> {
        match self {
            TreeItem::Inactive { path, .. } => Some(path.clone()),
            TreeItem::Project { name, .. } => by_project
                .get(*name)
                .and_then(|s| s.first())
                .map(|s| s.cwd.clone()),
            TreeItem::Session { session, .. } => Some(session.cwd.clone()),
            TreeItem::Separator { .. } => None,
            TreeItem::Run { coordinator, .. } => coordinator.map(|s| s.cwd.clone()),
            TreeItem::RunAgent { session, .. } => session.map(|s| s.cwd.clone()),
        }
    }
}

/// Tokens as the row shows them: rounded to thousands, not truncated. Node used
/// `(total / 1000).toFixed(0)`, and the render-gating signature in `store.js`
/// rounded to the same granularity on purpose — so a working agent only redraws
/// when the *displayed* number moves.
fn thousands(total: u64) -> u64 {
    (total as f64 / 1000.0).round() as u64
}

fn total_tokens(sessions: &[Session]) -> u64 {
    sessions
        .iter()
        .filter_map(|s| s.last_usage.as_ref())
        .map(|u| u.total_tokens())
        .sum()
}

/// Build the tree. With no configured groups this is the flat project list
/// (Node's `buildFlatTree`); with groups it is the grouped one. Node had two
/// functions; the flat form is just the grouped form with one implicit group,
/// so the shared row emission lives in [`emit_project`].
pub fn build_grouped_tree<'a>(
    by_project: &'a SessionsByProject,
    expanded: &HashSet<String>,
    groups: &'a [Group],
    discovered: &'a BTreeMap<String, Vec<String>>,
) -> Vec<TreeItem<'a>> {
    build_grouped_tree_with(by_project, expanded, groups, discovered, true, &[])
}

/// The same, told whether to draw a group's quiet folders.
///
/// They are off by default: a group of eighteen checkouts draws eighteen grey
/// rows and buries the two projects actually running. They earn their place
/// only when you want to START something in a quiet repo, which `F` reveals.
pub fn build_grouped_tree_with<'a>(
    by_project: &'a SessionsByProject,
    expanded: &HashSet<String>,
    groups: &'a [Group],
    discovered: &'a BTreeMap<String, Vec<String>>,
    show_inactive: bool,
    runs: &'a [RunSection<'a>],
) -> Vec<TreeItem<'a>> {
    let mut items = Vec::new();
    // Runs first, above the group separators. A run is what the reviewer came
    // to look at while one is open, and the section disappears entirely when
    // none is — so it costs a quiet dashboard nothing.
    emit_runs(&mut items, runs, expanded);
    if groups.is_empty() {
        for (name, sessions) in by_project {
            emit_project(&mut items, name, sessions, expanded);
        }
        return items;
    }

    let mut claimed: HashSet<&str> = HashSet::new();

    for (gi, group) in groups.iter().enumerate() {
        let group_path = trim_trailing_separators(&group.path);
        items.push(TreeItem::Separator {
            name: &group.name,
            group_index: Some(gi),
        });

        // Which immediate subdirectory of the group each live project sits in.
        // Keyed by directory name, not project name, so two checkouts of the
        // same repo under one group stay separate rows.
        let mut active: BTreeMap<&str, (&'a str, Vec<&'a Session>)> = BTreeMap::new();
        for (name, sessions) in by_project {
            if claimed.contains(name.as_str()) {
                continue;
            }
            let matching: Vec<&Session> = sessions
                .iter()
                .filter(|s| child_dir_of(&s.cwd, group_path).is_some())
                .collect();
            let Some(first) = matching.first() else {
                continue;
            };
            let dir_name = child_dir_of(&first.cwd, group_path).unwrap_or_default();
            let entry = active
                .entry(dir_name)
                .or_insert((name.as_str(), Vec::new()));
            entry.1.extend(matching);
            claimed.insert(name.as_str());
        }

        // Live folders first, then quiet ones. Otherwise a running session gets
        // buried in an alphabetical list of idle checkouts and scrolls out of
        // view, which reads as the scanner having missed it.
        for (project, sessions) in active.values() {
            let expanded_now = expanded.contains(*project);
            items.push(TreeItem::Project {
                name: project,
                session_count: sessions.len(),
                total_tokens: sessions
                    .iter()
                    .filter_map(|s| s.last_usage.as_ref())
                    .map(|u| u.total_tokens())
                    .sum(),
                expanded: expanded_now,
            });
            if expanded_now {
                for session in sessions {
                    items.push(TreeItem::Session {
                        project_name: project,
                        session,
                    });
                }
            }
        }

        if !show_inactive {
            continue;
        }
        for dir in discovered.get(group_path).map(Vec::as_slice).unwrap_or(&[]) {
            if active.contains_key(dir.as_str()) {
                continue;
            }
            items.push(TreeItem::Inactive {
                name: dir,
                path: join_dir(group_path, dir),
            });
        }
    }

    // Anything running outside every configured group still has to be reachable.
    let unclaimed: Vec<&String> = by_project
        .keys()
        .filter(|name| !claimed.contains(name.as_str()))
        .collect();
    if !unclaimed.is_empty() {
        items.push(TreeItem::Separator {
            name: "Other sessions",
            group_index: None,
        });
        for name in unclaimed {
            emit_project(&mut items, name, &by_project[name], expanded);
        }
    }
    items
}

/// Emit the Runs section, or nothing at all when no run is open.
fn emit_runs<'a>(
    items: &mut Vec<TreeItem<'a>>,
    runs: &'a [RunSection<'a>],
    expanded: &HashSet<String>,
) {
    if runs.is_empty() {
        return;
    }
    items.push(TreeItem::Separator {
        name: "Runs",
        // Not a configured group, so `d` cannot remove it — the same contract
        // the "Other sessions" divider has.
        group_index: None,
    });
    for run in runs {
        let key = format!("r:{}", run.run_id);
        let is_expanded = expanded.contains(&key);
        items.push(TreeItem::Run {
            run_id: run.run_id,
            stage: run.stage,
            coordinator: run.coordinator,
            agents: run.agents.len(),
            asking: run.agents.iter().filter(|agent| agent.asking).count(),
            expanded: is_expanded,
        });
        if !is_expanded {
            continue;
        }
        for agent in &run.agents {
            items.push(TreeItem::RunAgent {
                run_id: run.run_id,
                task_id: agent.task_id,
                session: agent.session,
                asking: agent.asking,
            });
        }
    }
}

fn emit_project<'a>(
    items: &mut Vec<TreeItem<'a>>,
    name: &'a str,
    sessions: &'a [Session],
    expanded: &HashSet<String>,
) {
    let is_expanded = expanded.contains(name);
    items.push(TreeItem::Project {
        name,
        session_count: sessions.len(),
        total_tokens: total_tokens(sessions),
        expanded: is_expanded,
    });
    if is_expanded {
        for session in sessions {
            items.push(TreeItem::Session {
                project_name: name,
                session,
            });
        }
    }
}

fn gray() -> Style {
    Style::default().fg(color_from_name("gray"))
}

/// Format one row. `config` supplies nicknames from the cached handle — brief
/// §10 mandate #3: Node re-read `~/.claude-sessions.json` here, once per
/// rendered row per frame.
pub fn format_tree_item(item: &TreeItem<'_>, config: &ConfigHandle) -> Line<'static> {
    match item {
        TreeItem::Separator { name, .. } => {
            let dashes = "─".repeat(40);
            Line::from(Span::styled(
                format!("── {name} {dashes}"),
                Style::default().add_modifier(Modifier::BOLD),
            ))
        }
        TreeItem::Inactive { name, .. } => Line::from(Span::styled(format!("  📁 {name}"), gray())),
        TreeItem::Project {
            name,
            session_count,
            total_tokens,
            expanded,
        } => {
            let arrow = if *expanded { "▼" } else { "▶" };
            let sessions = if *session_count == 1 {
                "1 session".to_string()
            } else {
                format!("{session_count} sessions")
            };
            let tokens = if *total_tokens > 0 {
                format!("{}K tokens", thousands(*total_tokens))
            } else {
                String::new()
            };
            Line::from(vec![
                Span::raw(format!("{arrow} 📁 ")),
                Span::styled(
                    (*name).to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {sessions}"), gray()),
                Span::styled(format!("  {tokens}"), gray()),
            ])
        }
        TreeItem::Session { session, .. } => format_session_row(session, config),
        TreeItem::Run {
            stage,
            coordinator,
            agents,
            asking,
            expanded,
            ..
        } => {
            let arrow = if *expanded { "▼" } else { "▶" };
            // The coordinator's own state, because a run whose coordinator has
            // died still shows its QA sessions working and looks healthy.
            let (dot, state) = match coordinator {
                Some(session) => (
                    Style::default().fg(color_of(session.status.color())),
                    session.status.label().to_string(),
                ),
                None => (gray(), "no coordinator".to_string()),
            };
            let agents = if *agents == 1 {
                "1 agent".to_string()
            } else {
                format!("{agents} agents")
            };
            // Only when it is not zero: a column reading "0 asking" on every
            // quiet run trains people to stop reading it.
            let waiting = if *asking > 0 {
                Span::styled(
                    format!("  {asking} asking"),
                    Style::default()
                        .fg(color_from_name("yellow"))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(String::new())
            };
            Line::from(vec![
                Span::raw(format!("{arrow} ")),
                Span::styled("●", dot),
                Span::styled(
                    format!(" {}", truncate(stage, 24)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("  coordinator".to_string(), gray()),
                Span::styled(format!("  {agents}"), gray()),
                waiting,
                Span::styled(format!("  {state}"), gray()),
            ])
        }
        TreeItem::RunAgent {
            task_id,
            session,
            asking,
            ..
        } => {
            // An indented session row, so a run's agents read as the same kind
            // of thing as every other session in this list.
            let Some(session) = session else {
                return Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("#{task_id}"), gray()),
                    Span::styled("  not started".to_string(), gray()),
                ]);
            };
            let mut line = format_session_row(session, config);
            line.spans.insert(0, Span::raw("  "));
            if *asking {
                line.spans.push(Span::styled(
                    "  ← asks you".to_string(),
                    Style::default()
                        .fg(color_from_name("yellow"))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            line
        }
    }
}

fn format_session_row(session: &Session, config: &ConfigHandle) -> Line<'static> {
    let status = session.status;
    let style = Style::default().fg(color_of(status.color()));

    // A working session says what it is doing; anything else says what it is.
    let label = match (status, session.activity_detail.as_str()) {
        (SessionStatus::Working | SessionStatus::Compacting, detail) if !detail.is_empty() => {
            format!("{detail}...")
        }
        _ => status.label().to_string(),
    };

    // The LAST message's usage: what the session is holding now, not the sum of
    // every turn it has ever run.
    //
    // No context percentage. It was a share of a fixed 200K window, which stopped
    // meaning anything once the header gained a real usage readout, and the space
    // now carries the task number — the thing people actually ask of this list:
    // "which task is that?".
    let tokens = match session.last_usage.as_ref().map(|u| u.total_tokens()) {
        Some(total) if total > 0 => format!("{}K", thousands(total)),
        _ => String::new(),
    };

    // A session with no task shows nothing here, and plenty legitimately have
    // none: a hand-started session, a merge-conflict run, the dashboard's own.
    let task = match session.task_id {
        Some(id) => format!("#{id}"),
        None => String::new(),
    };

    // The plain text is kept alongside the styled span: the column after this
    // one is padded against its DISPLAY width, and a Span cannot be measured
    // without unwrapping it again.
    let (name_text, name) = match config.session_nickname(&session.session_id) {
        Some(nickname) => {
            let text = truncate(nickname, 20);
            let span = Span::styled(
                text.clone(),
                Style::default()
                    .fg(color_from_name("white"))
                    .add_modifier(Modifier::BOLD),
            );
            (text, span)
        }
        None if session.starting => (
            "new".to_string(),
            Span::styled(
                "new".to_string(),
                Style::default().fg(color_from_name("cyan")),
            ),
        ),
        None => {
            let text: String = session.session_id.chars().take(4).collect();
            (text.clone(), Span::raw(text))
        }
    };

    // Status goes LAST, and every column before it is padded to a fixed width.
    //
    // Status is the only field whose text changes length constantly — "idle" one
    // second, "Editing session-row.test.js..." the next. Anything to its right
    // moved every time it changed, so the whole line jittered. With it at the
    // end, only its own tail moves and the columns you read stay put.
    //
    // The git branch used to sit at the end in magenta. It is gone: the task
    // number says the same thing in less space, and every QA branch for one task
    // looks like every other.
    //
    // Padding is measured on the DISPLAY width, never the byte length — a
    // nickname can hold multi-byte glyphs, and counting bytes would push every
    // column out by their length.
    Line::from(vec![
        Span::styled("    ●", style),
        Span::raw(" "),
        name,
        Span::raw(column_gap(&name_text, 10)),
        Span::styled(task.clone(), Style::default().fg(color_from_name("green"))),
        Span::raw(column_gap(&task, 8)),
        Span::styled(tokens.clone(), gray()),
        Span::raw(column_gap(&tokens, 7)),
        Span::styled(label, style),
    ])
}

/// An owned snapshot of whichever row the cursor is on.
///
/// [`TreeItem`] borrows the state it was built from, so a key handler cannot
/// hold one while it mutates that state. This is the handful of fields the
/// handlers actually need, copied out before the borrow ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedRow {
    Separator {
        name: String,
        group_index: Option<usize>,
    },
    Project {
        name: String,
    },
    Session {
        session_id: String,
        pids: Vec<u32>,
    },
    Inactive {
        path: String,
    },
    /// A run header. Enter expands it, and the run menu is still reached from
    /// the board — this row is a view, not a second control surface.
    Run {
        run_id: String,
    },
}

impl TreeItem<'_> {
    pub fn to_selected(&self) -> SelectedRow {
        match self {
            TreeItem::Separator { name, group_index } => SelectedRow::Separator {
                name: (*name).to_string(),
                group_index: *group_index,
            },
            TreeItem::Project { name, .. } => SelectedRow::Project {
                name: (*name).to_string(),
            },
            TreeItem::Session { session, .. } => SelectedRow::Session {
                session_id: session.session_id.clone(),
                pids: session.pids.clone(),
            },
            TreeItem::Inactive { path, .. } => SelectedRow::Inactive { path: path.clone() },
            TreeItem::Run { run_id, .. } => SelectedRow::Run {
                run_id: (*run_id).to_string(),
            },
            // A run's agent IS a session, and everything that works on a
            // session row works here: view it, focus its terminal, nickname it.
            TreeItem::RunAgent { session, .. } => match session {
                Some(session) => SelectedRow::Session {
                    session_id: session.session_id.clone(),
                    pids: session.pids.clone(),
                },
                None => SelectedRow::Separator {
                    name: String::new(),
                    group_index: None,
                },
            },
        }
    }
}

/// The spaces that carry a column out to a fixed width, always at least one.
///
/// Measured on the display width rather than the byte length: see the note on
/// the session row.
fn column_gap(text: &str, width: usize) -> String {
    let used = unicode_width::UnicodeWidthStr::width(text);
    " ".repeat(width.saturating_sub(used).max(1))
}
