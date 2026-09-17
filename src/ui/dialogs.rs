//! Modal dialogs: the primitives, the dialogs built from them, and the one
//! exhaustive `match` that dispatches between them.
//!
//! The Node app kept all 28 dialogs plus their primitives in a single 1457-line
//! `dialogs.js` and dispatched on a string in a `switch` whose `default` was
//! silence — a typo'd dialog name simply did nothing. WORKING.md rule 4 splits
//! the file by area; the dispatcher below is an enum, so a dialog that exists
//! and is not handled will not compile.
//!
//! Split by area: [`session`] is the sessions tree's, [`board`] / [`folders`] /
//! [`odoo`] / [`pipeline`] belong to the board tab, and [`deploy`] /
//! [`deploy_config`] / [`merge`] / [`deploy_confirm`] / [`mrs`] to the Deploy
//! tab — its menus, its config editor, its two merge confirmations, its deploy
//! and conflict confirmations, and the open-merge-request browser.

pub mod board;
pub mod deploy;
pub mod deploy_config;
pub mod deploy_confirm;
pub mod folders;
pub mod gates;
pub mod log;
pub mod merge;
pub mod mrs;
pub mod odoo;
pub mod pipeline;
pub mod project_filter;
pub mod purge;
pub mod session;
pub mod settings;
pub mod shutdown;
pub mod viewer;
pub mod widgets;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::config::ConfigHandle;
use crate::ui::board::{SessionTarget, StartRequest};
use crate::ui::state::{Action, Quit};

pub use board::{ContextDialog, RunAction, RunCommand, RunMenu, TaskAction, TaskMenu};
pub use deploy::{DeployAction, DeployMenu, DeployTaskAction, DeployTaskMenu};
pub use deploy_config::{DeployConfig, DeployField};
pub use deploy_confirm::{DeployConfirm, ResolveConflictConfirm};
pub use folders::{DirPicker, FolderManager, SavedDirPicker, TargetBranch};
pub use gates::{AlreadyRunning, BlockedBy};
pub use log::{DaemonLogs, LogViewer};
pub use merge::{MergeAllConfirm, MergeConfirm, MERGE_ALL_LISTED};
pub use mrs::OpenMrs;
pub use odoo::{NotifMenu, Remote, StagePicker};
pub use pipeline::PipelineView;
pub use project_filter::{ProjectFilter, ProjectPick};
pub use purge::PurgeConfirm;
pub use session::{AddGroup, KillConfirm, Rename, Search};
pub use settings::SettingsDialog;
pub use shutdown::ShutdownConfirm;
pub use viewer::{FileViewer, ViewerOutcome};
pub use widgets::{InlineChoice, ListOutcome, PromptOutcome, SelectList, TextPrompt};

pub use crate::ui::actions::BoardData;

/// A folder the user picked, and the launch (if any) that was waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedDir {
    pub project: String,
    pub dir: String,
    pub request: Option<Box<StartRequest>>,
}

/// A task-menu entry that the board, not the dialog, knows how to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommand {
    pub task: Box<crate::types::Task>,
    pub action: TaskAction,
}

/// The same for the Deploy tab's project menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub project: String,
    pub action: DeployAction,
}

/// …and for its task menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployTaskCommand {
    pub task: Box<crate::types::DeployTask>,
    pub action: DeployTaskAction,
}

/// What a dialog did with a key.
///
/// A channel rather than a `&mut AppState`: the dispatcher owns every state
/// change, so a dialog cannot replace itself halfway through a key and leave
/// the caller about to put the old one back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Keep the dialog open.
    Stay,
    Close,
    /// Close, and hand this to the action queue.
    Act(Action),
    /// Close the dashboard.
    Quit(Quit),
    /// Close and say something in the detail pane.
    Flash(String),
    /// Run this and/or say this, and KEEP the dialog open.
    ///
    /// The pipeline viewer is a workspace rather than a confirmation: `t`
    /// writes the starter override and `e` opens an editor on it, and both
    /// leave you looking at the flow they just changed.
    Keep {
        action: Option<Action>,
        flash: Option<String>,
    },
    /// Close and act on a QA run.
    Run(Box<RunCommand>),
    /// Close and run the shared start flow, which owns the two guards, the
    /// folder resolution and the prompt assembly.
    Start(Box<StartRequest>),
    /// A folder was chosen: remember it, then continue whatever was waiting.
    PickDir(Box<PickedDir>),
    /// "Add another folder" from the saved-folder picker.
    AddFolder(Box<StartRequest>),
    /// A task-menu entry the board carries out.
    Task(Box<TaskCommand>),
    /// Close, switch to the sessions view and open this conversation.
    GoTo(Box<SessionTarget>),
    /// The board's project filter changed: what is on screen answers the old
    /// one, so it is dropped and refetched.
    FilterChanged,
    /// A Deploy-tab project-menu entry.
    Deploy(Box<DeployCommand>),
    /// A Deploy-tab task-menu entry.
    DeployTask(Box<DeployTaskCommand>),
    /// Close and hand this task's conflicted MR to an agent.
    ResolveConflicts(Box<crate::types::DeployTask>),
    /// Close and open the deploy config for this project — what the
    /// unconfigured deploy confirmation's second button does.
    Configure(String),
}

/// What a dialog is allowed to reach while handling a key.
///
/// Deliberately narrow: config, which dialogs legitimately edit (nicknames,
/// groups, repo folders, target branches), and nothing else. A dialog cannot
/// spawn, signal or read the session list — those arrive as constructor
/// arguments or leave as a [`DialogOutcome`].
pub struct DialogCtx<'a> {
    pub config: &'a mut ConfigHandle,
}

/// One dialog at a time, taken out of the state and put back on every frame —
/// so the enum's size is a per-frame move, and one variant being three times
/// the rest is worth an indirection. [`LogViewer`] is that variant: it carries a
/// whole [`Paths`](crate::paths::Paths), which is sixteen `PathBuf`s, and a
/// `PathBuf` is a third larger on Windows than on Unix.
#[derive(Debug, Clone)]
pub enum Dialog {
    Kill(KillConfirm),
    Rename(Rename),
    AddGroup(AddGroup),
    Search(Search),
    Settings(SettingsDialog),
    Shutdown(ShutdownConfirm),
    FileViewer(FileViewer),
    LogViewer(Box<LogViewer>),
    PurgeConfirm(PurgeConfirm),
    // --- board ---
    TaskMenu(TaskMenu),
    RunMenu(RunMenu),
    Context(ContextDialog),
    BlockedBy(BlockedBy),
    AlreadyRunning(AlreadyRunning),
    StagePicker(StagePicker),
    NotifMenu(NotifMenu),
    ProjectFilter(ProjectFilter),
    DirPicker(DirPicker),
    SavedDirPicker(SavedDirPicker),
    FolderManager(FolderManager),
    TargetBranch(TargetBranch),
    Pipeline(PipelineView),
    DaemonLogs(DaemonLogs),
    OpenMrs(OpenMrs),
    // --- deploy ---
    DeployMenu(DeployMenu),
    DeployTaskMenu(DeployTaskMenu),
    MergeConfirm(MergeConfirm),
    MergeAllConfirm(MergeAllConfirm),
    DeployConfirm(DeployConfirm),
    DeployConfig(DeployConfig),
    ResolveConflict(ResolveConflictConfirm),
}

impl Dialog {
    /// A modal swallows input: the view beneath it never sees the key. That is
    /// what stops `q` quitting the dashboard from inside a confirmation.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        area: Rect,
        ctx: &mut DialogCtx<'_>,
    ) -> DialogOutcome {
        match self {
            Dialog::Kill(dialog) => dialog.handle_key(key, ctx),
            Dialog::Rename(dialog) => dialog.handle_key(key, ctx),
            Dialog::AddGroup(dialog) => dialog.handle_key(key, ctx),
            Dialog::Search(dialog) => dialog.handle_key(key, ctx),
            Dialog::Settings(dialog) => dialog.handle_key(key, ctx),
            Dialog::Shutdown(dialog) => dialog.handle_key(key),
            Dialog::FileViewer(dialog) => match dialog.handle_key(key, area) {
                ViewerOutcome::Stay => DialogOutcome::Stay,
                ViewerOutcome::Close => DialogOutcome::Close,
                ViewerOutcome::Open(path) => {
                    DialogOutcome::Act(Action::OpenEditor { path, line: 1 })
                }
            },
            Dialog::LogViewer(dialog) => dialog.handle_key(key, area, ctx),
            Dialog::PurgeConfirm(dialog) => dialog.handle_key(key, ctx),
            Dialog::DaemonLogs(dialog) => dialog.handle_key(key, area, ctx),
            Dialog::TaskMenu(dialog) => dialog.handle_key(key, ctx),
            Dialog::RunMenu(dialog) => dialog.handle_key(key, ctx),
            Dialog::Context(dialog) => dialog.handle_key(key, ctx),
            Dialog::BlockedBy(dialog) => dialog.handle_key(key, ctx),
            Dialog::AlreadyRunning(dialog) => dialog.handle_key(key, ctx),
            Dialog::StagePicker(dialog) => dialog.handle_key(key, ctx),
            Dialog::NotifMenu(dialog) => dialog.handle_key(key, ctx),
            Dialog::ProjectFilter(dialog) => dialog.handle_key(key, ctx),
            Dialog::DirPicker(dialog) => dialog.handle_key(key, ctx),
            Dialog::SavedDirPicker(dialog) => dialog.handle_key(key, ctx),
            Dialog::FolderManager(dialog) => dialog.handle_key(key, ctx),
            Dialog::TargetBranch(dialog) => dialog.handle_key(key, ctx),
            Dialog::Pipeline(dialog) => dialog.handle_key(key, ctx),
            Dialog::OpenMrs(dialog) => dialog.handle_key(key, ctx),
            Dialog::DeployMenu(dialog) => dialog.handle_key(key, ctx),
            Dialog::DeployTaskMenu(dialog) => dialog.handle_key(key, ctx),
            Dialog::MergeConfirm(dialog) => dialog.handle_key(key, ctx),
            Dialog::MergeAllConfirm(dialog) => dialog.handle_key(key, ctx),
            Dialog::DeployConfirm(dialog) => dialog.handle_key(key, ctx),
            Dialog::DeployConfig(dialog) => dialog.handle_key(key, ctx),
            Dialog::ResolveConflict(dialog) => dialog.handle_key(key, ctx),
        }
    }

    /// Hand a dialog the data it asked for. `false` means the answer was not
    /// this dialog's — a lookup that landed after the cursor moved on.
    pub fn accept(&mut self, data: &BoardData) -> bool {
        match self {
            Dialog::BlockedBy(dialog) => dialog.accept(data),
            Dialog::StagePicker(dialog) => dialog.accept(data),
            Dialog::ProjectFilter(dialog) => dialog.accept(data),
            Dialog::PurgeConfirm(dialog) => dialog.accept(data),
            Dialog::TaskMenu(dialog) => dialog.accept(data),
            Dialog::OpenMrs(dialog) => dialog.accept(data),
            _ => false,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, config: &ConfigHandle) {
        match self {
            Dialog::Kill(dialog) => dialog.render(frame, area),
            Dialog::Rename(dialog) => dialog.render(frame, area),
            Dialog::AddGroup(dialog) => dialog.render(frame, area),
            Dialog::Search(dialog) => dialog.render(frame, area),
            Dialog::Settings(dialog) => dialog.render(frame, area, config.chat()),
            Dialog::Shutdown(dialog) => dialog.render(frame, area),
            Dialog::FileViewer(dialog) => {
                // The only I/O in a render path, and it is a stat: the file is
                // re-read only when its mtime moved (brief §10 mandate #8).
                dialog.reload_if_changed();
                dialog.render(frame, area);
            }
            Dialog::LogViewer(dialog) => dialog.render(frame, area),
            Dialog::PurgeConfirm(dialog) => dialog.render(frame, area),
            Dialog::DaemonLogs(dialog) => dialog.render(frame, area),
            Dialog::TaskMenu(dialog) => dialog.render(frame, area),
            Dialog::RunMenu(dialog) => dialog.render(frame, area),
            Dialog::Context(dialog) => dialog.render(frame, area),
            Dialog::BlockedBy(dialog) => dialog.render(frame, area),
            Dialog::AlreadyRunning(dialog) => dialog.render(frame, area),
            Dialog::StagePicker(dialog) => dialog.render(frame, area),
            Dialog::NotifMenu(dialog) => dialog.render(frame, area),
            Dialog::ProjectFilter(dialog) => dialog.render(frame, area),
            Dialog::DirPicker(dialog) => dialog.render(frame, area),
            Dialog::SavedDirPicker(dialog) => dialog.render(frame, area),
            Dialog::FolderManager(dialog) => dialog.render(frame, area),
            Dialog::TargetBranch(dialog) => dialog.render(frame, area),
            Dialog::Pipeline(dialog) => dialog.render(frame, area),
            Dialog::OpenMrs(dialog) => dialog.render(frame, area),
            Dialog::DeployMenu(dialog) => dialog.render(frame, area),
            Dialog::DeployTaskMenu(dialog) => dialog.render(frame, area),
            Dialog::MergeConfirm(dialog) => dialog.render(frame, area),
            Dialog::MergeAllConfirm(dialog) => dialog.render(frame, area),
            Dialog::DeployConfirm(dialog) => dialog.render(frame, area),
            Dialog::DeployConfig(dialog) => dialog.render(frame, area),
            Dialog::ResolveConflict(dialog) => dialog.render(frame, area),
        }
    }

    /// The name used in flashes and tests. Matches the Node dialog registry's
    /// string keys, so the smoke matrix reads the same in both codebases.
    pub fn name(&self) -> &'static str {
        match self {
            Dialog::Kill(_) => "killConfirm",
            Dialog::Rename(_) => "rename",
            Dialog::AddGroup(_) => "addGroup",
            Dialog::Search(_) => "search",
            Dialog::Settings(_) => "settings",
            Dialog::Shutdown(_) => "shutdown",
            Dialog::FileViewer(_) => "fileViewer",
            Dialog::LogViewer(_) => "logViewer",
            Dialog::PurgeConfirm(_) => "purgeConfirm",
            Dialog::DaemonLogs(_) => "daemonLogs",
            Dialog::TaskMenu(_) => "taskMenu",
            Dialog::RunMenu(_) => "runMenu",
            Dialog::Context(_) => "contextDialog",
            Dialog::BlockedBy(_) => "blockedBy",
            Dialog::AlreadyRunning(_) => "alreadyRunning",
            Dialog::StagePicker(_) => "stagePicker",
            Dialog::NotifMenu(_) => "notifMenu",
            Dialog::ProjectFilter(_) => "projectFilter",
            Dialog::DirPicker(_) => "dirPicker",
            Dialog::SavedDirPicker(_) => "savedDirPicker",
            Dialog::FolderManager(_) => "folderManager",
            Dialog::TargetBranch(_) => "targetBranch",
            Dialog::Pipeline(_) => "pipeline",
            Dialog::OpenMrs(_) => "openMRs",
            Dialog::DeployMenu(_) => "deployMenu",
            Dialog::DeployTaskMenu(_) => "deployTaskMenu",
            Dialog::MergeConfirm(_) => "mergeConfirm",
            Dialog::MergeAllConfirm(_) => "mergeAllConfirm",
            Dialog::DeployConfirm(_) => "deployConfirm",
            Dialog::DeployConfig(_) => "deployConfig",
            Dialog::ResolveConflict(_) => "resolveConflictConfirm",
        }
    }
}
