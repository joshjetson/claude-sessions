//! The dialogs the sessions tree opens: kill, rename, add group, search.
//!
//! Ported from `KillConfirm` / `Rename` / `AddGroup` / `Search` in the Node
//! app's `src/tui/dialogs.js`. Each is a small value with a key handler; none of
//! them spawns or signals anything itself — killing is an [`Action`] so the
//! draw thread never blocks on a process (brief §10 mandate #9), and so a test
//! can watch the action queue instead of watching for corpses.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::config::ConfigHandle;
use crate::types::Session;
use crate::ui::dialogs::widgets::{dialog_width, hint, render_modal, PromptOutcome, TextPrompt};
use crate::ui::dialogs::{DialogCtx, DialogOutcome};
use crate::ui::state::Action;
use crate::ui::theme::color_from_name;
use crate::ui::tree::{SelectedRow, SessionsByProject};

// --- kill -------------------------------------------------------------------

/// SIGTERM one session, or every session in a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillConfirm {
    pub pids: Vec<u32>,
    pub label: String,
    /// The run this kill ends, when the row was a run. Confirming it removes
    /// the run as well as the processes.
    pub stops_run: Option<String>,
}

impl KillConfirm {
    /// Build from the selected row. A row that owns no processes still opens the
    /// dialog — it says there is nothing to kill, rather than doing nothing when
    /// `x` is pressed, which reads as a broken key.
    pub fn for_row(
        row: Option<&SelectedRow>,
        by_project: &SessionsByProject,
        config: &ConfigHandle,
    ) -> Self {
        match row {
            Some(SelectedRow::Session { session_id, pids }) => KillConfirm {
                stops_run: None,
                pids: pids.clone(),
                label: match config.session_nickname(session_id) {
                    Some(nickname) => nickname.to_string(),
                    None => format!("session {}", session_id.chars().take(8).collect::<String>()),
                },
            },
            Some(SelectedRow::Project { name }) => {
                let sessions = by_project.get(name).map(Vec::as_slice).unwrap_or(&[]);
                KillConfirm {
                    stops_run: None,
                    pids: sessions
                        .iter()
                        .flat_map(|s| s.pids.iter().copied())
                        .collect(),
                    label: format!("all {} session(s) in {name}", sessions.len()),
                }
            }
            _ => KillConfirm {
                stops_run: None,
                pids: Vec::new(),
                label: String::new(),
            },
        }
    }

    /// Every process a QA run is holding: the coordinator and each agent.
    ///
    /// Built from the sessions that are LIVE right now, not from the run's task
    /// list. A task the run never started, or whose session has already gone,
    /// contributes nothing to kill — and counting it would make the dialog
    /// promise more than it does.
    pub fn for_run(
        run_id: &str,
        label: &str,
        coordinator: Option<&Session>,
        agents: &[&Session],
    ) -> Self {
        let mut pids: Vec<u32> = Vec::new();
        let mut live = 0;
        for session in coordinator.into_iter().chain(agents.iter().copied()) {
            pids.extend(session.pids.iter().copied());
            live += 1;
        }
        // The coordinator is named separately because killing it is the part
        // with a consequence the agents do not have: the run stops being
        // answered, and the agents that survive go back to asking the reviewer.
        let what = match (coordinator.is_some(), live) {
            (_, 0) => "nothing — no session in this run is running".to_string(),
            (true, 1) => "the coordinator".to_string(),
            (true, n) => format!("the coordinator and {} agent(s)", n - 1),
            (false, n) => format!("{n} agent(s), with no coordinator running"),
        };
        KillConfirm {
            pids,
            label: format!("{what} in {label}"),
            // Killing a run ends it. Leaving the row behind with every session
            // gone would be a run you cannot act on and cannot get rid of.
            stops_run: Some(run_id.to_string()),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match key.code {
            KeyCode::Enter if !self.pids.is_empty() => DialogOutcome::Act(Action::Kill {
                pids: self.pids.clone(),
                label: self.label.clone(),
            }),
            KeyCode::Enter | KeyCode::Esc => DialogOutcome::Close,
            _ => DialogOutcome::Stay,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let body = if self.pids.is_empty() {
            hint("No running processes to kill.")
        } else {
            Line::from(Span::styled(
                format!("Kill {}?", self.label),
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ))
        };
        render_modal(
            frame,
            area,
            " Kill ",
            color_from_name("red"),
            vec![
                Line::default(),
                body,
                Line::default(),
                hint("Enter confirm    Esc cancel"),
            ],
            dialog_width(area, 60),
        );
    }
}

// --- rename -----------------------------------------------------------------

/// Give a session a name you will recognise two hours later.
#[derive(Debug, Clone)]
pub struct Rename {
    pub session_id: String,
    pub prompt: TextPrompt,
}

impl Rename {
    pub fn new(session_id: impl Into<String>, config: &ConfigHandle) -> Self {
        let session_id = session_id.into();
        let initial = config
            .session_nickname(&session_id)
            .unwrap_or("")
            .to_string();
        Rename {
            session_id,
            prompt: TextPrompt::new(initial, false),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.prompt.handle_key(key) {
            PromptOutcome::Stay => DialogOutcome::Stay,
            PromptOutcome::Cancel => DialogOutcome::Close,
            PromptOutcome::Submit(value) => {
                // An empty name clears the nickname — that is how you undo one.
                let _ = ctx
                    .config
                    .save_session_nickname(&self.session_id, Some(value.trim()));
                DialogOutcome::Close
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        self.prompt.render(frame, area, " Rename Session ");
    }
}

// --- add group --------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AddGroupStep {
    Name,
    Path { name: String },
}

/// Two prompts in sequence: the group's name, then the directory it stands for.
#[derive(Debug, Clone)]
pub struct AddGroup {
    pub step: AddGroupStep,
    pub prompt: TextPrompt,
}

impl Default for AddGroup {
    fn default() -> Self {
        AddGroup::new()
    }
}

impl AddGroup {
    pub fn new() -> Self {
        AddGroup {
            step: AddGroupStep::Name,
            prompt: TextPrompt::new("", false),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.prompt.handle_key(key) {
            PromptOutcome::Stay => DialogOutcome::Stay,
            PromptOutcome::Cancel => DialogOutcome::Close,
            PromptOutcome::Submit(value) => {
                let value = value.trim().to_string();
                match &self.step {
                    AddGroupStep::Name => {
                        if value.is_empty() {
                            return DialogOutcome::Close;
                        }
                        // The second prompt starts empty apart from `~/` — Node
                        // needed distinct React keys here or the path field
                        // inherited the group name that had just been typed.
                        self.step = AddGroupStep::Path { name: value };
                        self.prompt = TextPrompt::new("~/", false);
                        DialogOutcome::Stay
                    }
                    AddGroupStep::Path { name } => {
                        if !value.is_empty() {
                            let _ = ctx.config.add_group(name, &value);
                            // A new group changes which folders are worth
                            // discovering, so the next scan has to look again.
                            return DialogOutcome::Act(Action::Refresh);
                        }
                        DialogOutcome::Close
                    }
                }
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let title = match &self.step {
            AddGroupStep::Name => " Group name ".to_string(),
            AddGroupStep::Path { name } => format!(" Directory path for \"{name}\" "),
        };
        self.prompt.render(frame, area, &title);
    }
}

// --- search -----------------------------------------------------------------

/// Filter the conversation pane to messages containing a keyword.
#[derive(Debug, Clone)]
pub struct Search {
    pub prompt: TextPrompt,
}

impl Search {
    pub fn new(config: &ConfigHandle) -> Self {
        Search {
            prompt: TextPrompt::new(config.chat().search_keyword.clone(), false),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut DialogCtx<'_>) -> DialogOutcome {
        match self.prompt.handle_key(key) {
            PromptOutcome::Stay => DialogOutcome::Stay,
            PromptOutcome::Cancel => DialogOutcome::Close,
            PromptOutcome::Submit(value) => {
                let mut chat = ctx.config.chat().clone();
                chat.search_keyword = value.trim().to_string();
                let _ = ctx.config.save_chat_config(chat);
                DialogOutcome::Close
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        self.prompt.render(frame, area, " Search messages ");
    }
}
