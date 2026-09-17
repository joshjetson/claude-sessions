//! What one line of a Claude Code transcript amounts to, once the parser has
//! read it.
//!
//! The format is undocumented and gains new `type` values between releases, so
//! everything past the type is optional and unrecognised types are carried
//! through rather than dropped.

use serde::{Deserialize, Serialize};

/// Token counts exactly as a transcript entry carries them. Every field is
/// optional because older entries omit the cache counters entirely — and a
/// missing counter is not a zero anywhere it is rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

impl Usage {
    /// Everything that occupies the context window: prompt tokens, both cache
    /// counters, and the reply.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
            + self.output_tokens.unwrap_or(0)
    }
}

/// Usage summed across a whole transcript. Unlike [`Usage`] these are running
/// totals, so an absent counter really is zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CumulativeUsage {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

/// `entry.type` of a transcript line. Claude Code adds new values between
/// releases, so unrecognised ones are carried through as [`EntryKind::Other`]
/// rather than dropped — the status machine's fall-through depends on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum EntryKind {
    User,
    Assistant,
    System,
    Progress,
    Other(String),
}

impl EntryKind {
    pub fn as_str(&self) -> &str {
        match self {
            EntryKind::User => "user",
            EntryKind::Assistant => "assistant",
            EntryKind::System => "system",
            EntryKind::Progress => "progress",
            EntryKind::Other(s) => s,
        }
    }
}

impl From<String> for EntryKind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "user" => EntryKind::User,
            "assistant" => EntryKind::Assistant,
            "system" => EntryKind::System,
            "progress" => EntryKind::Progress,
            _ => EntryKind::Other(s),
        }
    }
}

impl From<&str> for EntryKind {
    fn from(s: &str) -> Self {
        EntryKind::from(s.to_string())
    }
}

impl From<EntryKind> for String {
    fn from(k: EntryKind) -> Self {
        match k {
            EntryKind::Other(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

/// `entry.data` on a `progress` line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressData {
    /// `bash_progress` / `agent_progress` / `hook_progress`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_name: Option<String>,
}

/// The projection of a transcript's trailing entry that the status machine and
/// the activity label need — nothing more.
///
/// [`crate::transcript`] builds these; keeping the view this narrow means status
/// detection can be exercised without a transcript on disk, and the daemon can
/// hold one per session without pinning whole message bodies in memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastEntry {
    pub kind: EntryKind,
    /// `turn_duration` on a `system` entry means the turn finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MessageRole>,
    /// False when the entry carried no `message` at all, which the activity
    /// label distinguishes from a message with no tool calls in it.
    pub has_message: bool,
    /// Names of the `tool_use` blocks, in the order they appear. The label uses
    /// the last one; the status machine only asks whether the list is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_uses: Vec<String>,
    /// This entry is a tool's OUTPUT rather than something a person typed.
    /// A `user` entry is either one or the other, and the status machine reads
    /// them completely differently.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_tool_result: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressData>,
}

impl Default for EntryKind {
    fn default() -> Self {
        EntryKind::Other(String::new())
    }
}

impl LastEntry {
    /// Convenience for the common `{ type: "..." }` shape in tests and for the
    /// bookkeeping entries that carry nothing else.
    pub fn of_kind(kind: impl Into<EntryKind>) -> Self {
        LastEntry {
            kind: kind.into(),
            ..Default::default()
        }
    }

    pub fn has_tool_use(&self) -> bool {
        !self.tool_uses.is_empty()
    }

    /// The agent asked the person a question, or put a plan up for approval.
    ///
    /// Unambiguous: control has been handed back. Checked ahead of every
    /// recency rule, because otherwise the row reads "working" for the first
    /// ten seconds of every question asked.
    pub fn awaits_user_decision(&self) -> bool {
        self.tool_uses
            .iter()
            .any(|name| matches!(name.as_str(), "AskUserQuestion" | "ExitPlanMode"))
    }

    /// A `user` entry that is a tool's output, not a person's input.
    pub fn is_tool_result(&self) -> bool {
        matches!(self.kind, EntryKind::User) && self.has_tool_result
    }
}

/// A human prompt pulled out of a transcript. `timestamp` stays the raw ISO
/// string the transcript carried — parsing happens at render time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    pub timestamp: String,
}

/// Everything [`crate::transcript`] reads out of one transcript file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedSession {
    pub session_id: String,
    pub git_branch: String,
    pub last_timestamp: String,
    pub last_usage: Option<Usage>,
    pub last_entry: Option<LastEntry>,
    pub cumulative_usage: CumulativeUsage,
    pub prompts: Vec<Prompt>,
}

/// The cheap half of [`ParsedSession`], for callers that only need to know how
/// far along a transcript is (the scanner's pairing, the stall detector).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetaLite {
    pub session_id: String,
    pub last_timestamp: String,
    pub last_usage: Option<Usage>,
    pub last_entry: Option<LastEntry>,
}

/// One rendered turn in the conversation pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub text: String,
    pub timestamp: String,
    pub has_tool_use: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}
