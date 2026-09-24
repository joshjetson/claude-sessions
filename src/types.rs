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
mod role;
mod session;
mod transcript;
mod ui;
mod wire;

pub use board::{qa_state_badge, Board, BoardProject, BoardStage, OdooCreds, QaStateBadge, Task};
pub use deploy::{
    DeployBoard, DeployProjectState, DeployRun, DeployRunStatus, DeployTask, MergeRequest,
};
pub use notify::{
    Notification, NotificationKind, NotificationLevel, NotificationStatus, QUIET_SESSIONS_ID,
};
pub use role::{NotificationPolicy, UserRole};
pub use session::{RawSession, Session, SessionFile, SessionStatus};
pub use transcript::{
    ConversationMessage, CumulativeUsage, EntryKind, LastEntry, MessageRole, ParsedSession,
    ProgressData, Prompt, SessionMetaLite, Usage,
};
pub use ui::{Color, DefaultView, TerminalDriverName};
