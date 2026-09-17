//! The shapes every other module agrees on.
//!
//! Ported from the Node app's `src/types.ts`, with the string unions turned into
//! enums. Anything that crosses a file or a wire boundary derives serde and keeps
//! the Node field names (camelCase) so archives, marker files and daemon payloads
//! written by either implementation stay readable by the other.
//!
//! Config *file* shapes deliberately live in [`crate::config`] instead: the
//! accessors there return the enums below, but the on-disk model keeps unknown
//! values as strings so a hand-written config never loses data on save.

mod board;
mod deploy;
mod notify;
mod session;
mod transcript;
mod ui;
mod wire;

pub use board::{Board, BoardProject, BoardStage, OdooCreds, Task};
pub use deploy::{
    DeployBoard, DeployProjectState, DeployRun, DeployRunStatus, DeployTask, MergeRequest,
};
pub use notify::{Notification, NotificationKind, NotificationLevel, NotificationStatus};
pub use session::{RawSession, Session, SessionFile, SessionStatus};
pub use transcript::{
    ConversationMessage, CumulativeUsage, EntryKind, LastEntry, MessageRole, ParsedSession,
    ProgressData, Prompt, SessionMetaLite, Usage,
};
pub use ui::{Color, DefaultView, TerminalDriverName};
