//! One line of a session `.jsonl`, and the two things every caller pulls out of
//! it: the human prompt (if it is one) and the trailing-entry projection the
//! status machine reads.
//!
//! Tolerant by design. The format is undocumented and gains fields, entry types
//! and shapes between Claude Code releases, so *every* field is optional and a
//! field whose type changed costs us that field, never the whole entry — an
//! entry silently dropped would move a live session to `idle`, which is the one
//! failure a dashboard must not have. Lines that are not JSON at all are skipped
//! exactly as Node's `try { JSON.parse } catch { continue }` did.

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::tool_use::ToolInput;
use crate::types::{EntryKind, LastEntry, MessageRole, ProgressData, Usage};

/// Read a field only when it has the shape we expect, and treat anything else
/// as absent.
///
/// `#[serde(untagged)]` is what makes this cheap *and* order-preserving: serde
/// buffers the value into its own content tree, whose maps are a `Vec` of pairs.
/// Going through `serde_json::Value` instead would sort object keys, and the
/// fallback arm of [`super::format_tool_use`] renders the *first* string
/// parameter of a tool call — so key order is behaviour, not presentation.
fn shape_or_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match ShapeOr::<T>::deserialize(deserializer)? {
        ShapeOr::Expected(value) => Some(value),
        ShapeOr::Unexpected(_) => None,
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ShapeOr<T> {
    Expected(T),
    /// `IgnoredAny` accepts every JSON value, so this arm never fails.
    Unexpected(IgnoredAny),
}

/// One transcript line.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Entry {
    #[serde(rename = "type", deserialize_with = "shape_or_none")]
    pub kind: Option<EntryKind>,
    /// `turn_duration` on a `system` line means the turn finished.
    #[serde(deserialize_with = "shape_or_none")]
    pub subtype: Option<String>,
    #[serde(deserialize_with = "shape_or_none")]
    pub session_id: Option<String>,
    #[serde(deserialize_with = "shape_or_none")]
    pub git_branch: Option<String>,
    #[serde(deserialize_with = "shape_or_none")]
    pub timestamp: Option<String>,
    /// `external` marks a line a human actually typed.
    #[serde(deserialize_with = "shape_or_none")]
    pub user_type: Option<String>,
    #[serde(deserialize_with = "shape_or_none")]
    pub message: Option<Message>,
    /// Present when a `user` line is a tool's output rather than a prompt. Kept
    /// as raw JSON because only its presence matters — and because Node tested
    /// it for truthiness, so `null`/`false`/`0`/`""` do not count.
    pub tool_use_result: Option<Value>,
    /// Set on the echo a sub-agent's tool call leaves in the parent transcript.
    /// The key really is spelled with a capitalised acronym.
    #[serde(rename = "sourceToolAssistantUUID", deserialize_with = "shape_or_none")]
    pub source_tool_assistant_uuid: Option<String>,
    /// Payload of a `progress` line.
    #[serde(deserialize_with = "shape_or_none")]
    pub data: Option<ProgressData>,
}

/// `entry.message` — the API-shaped payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Message {
    #[serde(deserialize_with = "shape_or_none")]
    pub role: Option<MessageRole>,
    pub content: Content,
    #[serde(deserialize_with = "shape_or_none")]
    pub usage: Option<Usage>,
}

/// `message.content`: a bare string on older/simpler turns, a block array on
/// everything modern.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(from = "ContentRepr")]
pub enum Content {
    #[default]
    Absent,
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ContentRepr {
    Text(String),
    Blocks(Vec<ContentBlock>),
    Absent(IgnoredAny),
}

impl From<ContentRepr> for Content {
    fn from(repr: ContentRepr) -> Self {
        match repr {
            ContentRepr::Text(s) => Content::Text(s),
            ContentRepr::Blocks(b) => Content::Blocks(b),
            ContentRepr::Absent(_) => Content::Absent,
        }
    }
}

/// One element of a content array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "BlockRepr")]
pub enum ContentBlock {
    /// A bare string element. `extractTextContent` returns it; the conversation
    /// builder ignores it, because it has no `type` to match on — a distinction
    /// the Node code made by accident and both behaviours are relied on.
    Raw(String),
    /// `{"type":"text","text":…}` — only when `text` really is a string.
    Text(String),
    /// `input` keeps its key order; see [`shape_or_none`].
    ToolUse {
        name: Option<String>,
        input: ToolInput,
    },
    /// `{"type":"tool_result"}`; the payload only matters when it is a string.
    ToolResult(Option<String>),
    /// `thinking`, `image`, and whatever ships next.
    Other,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BlockRepr {
    Raw(String),
    Tagged(TaggedBlock),
    Other(IgnoredAny),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TaggedBlock {
    Text {
        #[serde(default, deserialize_with = "shape_or_none")]
        text: Option<String>,
    },
    ToolUse {
        #[serde(default, deserialize_with = "shape_or_none")]
        name: Option<String>,
        #[serde(default)]
        input: ToolInput,
    },
    ToolResult {
        #[serde(default, deserialize_with = "shape_or_none")]
        content: Option<String>,
    },
    /// Every other tagged block, so a new block type never fails the line.
    #[serde(other)]
    Other,
}

impl From<BlockRepr> for ContentBlock {
    fn from(repr: BlockRepr) -> Self {
        match repr {
            BlockRepr::Raw(s) => ContentBlock::Raw(s),
            // A `text` block whose `text` is not a string is not text: Node's
            // `typeof item.text === 'string'` guard skipped it.
            BlockRepr::Tagged(TaggedBlock::Text { text: Some(t) }) => ContentBlock::Text(t),
            BlockRepr::Tagged(TaggedBlock::ToolUse { name, input }) => {
                ContentBlock::ToolUse { name, input }
            }
            BlockRepr::Tagged(TaggedBlock::ToolResult { content }) => {
                ContentBlock::ToolResult(content)
            }
            BlockRepr::Tagged(_) | BlockRepr::Other(_) => ContentBlock::Other,
        }
    }
}

/// The first renderable text in a content value — ported from `utils.ts`, where
/// it lived apart from the block types it reads.
///
/// Returns a slice, empty when nothing matched, and stops at the *first* match
/// even when that match is an empty string: callers test the result for
/// emptiness themselves, which is what Node's falsy check did.
pub fn extract_text_content(content: &Content) -> &str {
    match content {
        Content::Text(s) => s,
        Content::Absent => "",
        Content::Blocks(blocks) => {
            for block in blocks {
                match block {
                    ContentBlock::Raw(s) | ContentBlock::Text(s) => return s,
                    ContentBlock::ToolResult(Some(s)) => return s,
                    _ => {}
                }
            }
            ""
        }
    }
}

/// JavaScript truthiness, because the prompt filter was written against it:
/// `toolUseResult: null` (or `false`, `0`, `""`) did not mark a line as tool
/// output.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// The sentence Claude Code puts in a rejected tool call's result when the
/// person declined without saying how to go on. The agent stops, and the
/// interrupt marker follows it. A rejection that carries the person's
/// instructions says "the user said:" instead, and the agent carries on, so
/// that one is deliberately not matched.
const DENIAL_STOP: &str = "STOP what you are doing and wait for the user";

/// Text that means the turn ended with nothing more coming from the agent.
///
/// - `[Request interrupted by user]` and `[Request interrupted by user for
///   tool use]`: the person pressed Escape, or denied a permission prompt.
/// - [`DENIAL_STOP`]: the rejected tool call's own result.
/// - `<local-command-stdout>` and `<local-command-stderr>`: a local slash
///   command such as `/model` printed its output. The agent does not answer
///   these.
/// - `<bash-stdout>` and `<bash-stderr>`: the person ran a `!` shell command.
fn is_turn_ender(text: &str) -> bool {
    let head = text.trim_start();
    head.starts_with("[Request interrupted by user")
        || head.starts_with("<local-command-stdout>")
        || head.starts_with("<local-command-stderr>")
        || head.starts_with("<bash-stdout>")
        || head.starts_with("<bash-stderr>")
        || text.contains(DENIAL_STOP)
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.is_empty())
}

impl Entry {
    /// Parse one line, or `None` when it is not JSON. Never an error: a
    /// half-written trailing line is normal for a transcript being appended to.
    pub fn parse_line(line: &str) -> Option<Entry> {
        if line.trim().is_empty() {
            return None;
        }
        serde_json::from_str(line).ok()
    }

    pub fn is(&self, kind: EntryKind) -> bool {
        self.kind.as_ref() == Some(&kind)
    }

    pub fn session_id(&self) -> Option<&str> {
        non_empty(&self.session_id)
    }

    pub fn git_branch(&self) -> Option<&str> {
        non_empty(&self.git_branch)
    }

    pub fn timestamp(&self) -> &str {
        non_empty(&self.timestamp).unwrap_or("")
    }

    /// The usage counters of an assistant turn, which is the only place they
    /// appear.
    pub fn assistant_usage(&self) -> Option<&Usage> {
        if !self.is(EntryKind::Assistant) {
            return None;
        }
        self.message.as_ref()?.usage.as_ref()
    }

    /// The one implementation of the human-prompt filter.
    ///
    /// Node open-coded it three times (session parse, conversation build, and
    /// again in the combined parser); every caller here goes through this. The
    /// rules, in Node's order: a `user` line, with `message.role == "user"`, with
    /// `userType == "external"`, carrying neither a tool result nor a sub-agent
    /// echo, whose text is neither an interrupt marker nor a system-authored
    /// turn (those open with "The user").
    pub fn human_prompt(&self) -> Option<&str> {
        if !self.is(EntryKind::User) {
            return None;
        }
        let message = self.message.as_ref()?;
        if message.role != Some(MessageRole::User) {
            return None;
        }
        if self.user_type.as_deref() != Some("external") {
            return None;
        }
        if self.tool_use_result.as_ref().is_some_and(is_truthy) {
            return None;
        }
        if non_empty(&self.source_tool_assistant_uuid).is_some() {
            return None;
        }
        let text = extract_text_content(&message.content);
        if text.is_empty() || text.contains("[Request interrupted") || text.starts_with("The user")
        {
            return None;
        }
        Some(text)
    }

    /// The content blocks of an assistant turn, for the conversation builder.
    pub fn assistant_message(&self) -> Option<&Message> {
        if !self.is(EntryKind::Assistant) {
            return None;
        }
        let message = self.message.as_ref()?;
        (message.role == Some(MessageRole::Assistant)).then_some(message)
    }

    /// Names of the `tool_use` blocks, in order. A block with no name still
    /// counts — the status machine only asks whether the turn made a tool call.
    fn tool_use_names(&self) -> Vec<String> {
        let Some(Content::Blocks(blocks)) = self.message.as_ref().map(|m| &m.content) else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { name, .. } => Some(name.clone().unwrap_or_default()),
                _ => None,
            })
            .collect()
    }

    /// Whether any content block is a tool result.
    fn has_tool_result(&self) -> bool {
        let Some(Content::Blocks(blocks)) = self.message.as_ref().map(|m| &m.content) else {
            return false;
        };
        blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult(_)))
    }

    /// Whether this line is part of the conversation, and so may drive the
    /// session's status.
    ///
    /// An allowlist, not a denylist. Current Claude Code ends almost every
    /// transcript on bookkeeping — `attachment` (hook results, token
    /// reminders), `last-prompt`, `ai-title`, `mode`, `permission-mode`,
    /// `cost-state`, `file-history-snapshot`, `queue-operation`, and system
    /// lines such as `stop_hook_summary` and `away_summary` — and it adds new
    /// kinds between releases. When the status machine read whichever line was
    /// last, every one of those reported "idle", including the hook results
    /// written after each tool call and each tool result. A new bookkeeping
    /// kind now defaults to "ignored", which leaves the status alone, rather
    /// than to "idle", which is wrong while the agent works.
    ///
    /// The `system` subtypes kept are the ones that change whose turn it is:
    /// `turn_duration` (the turn finished), `compact_boundary` (a compaction
    /// finished) and `local_command` (a local slash command printed its
    /// output, and the agent does not reply to those).
    pub fn drives_status(&self) -> bool {
        match &self.kind {
            Some(EntryKind::User | EntryKind::Assistant | EntryKind::Progress) => true,
            Some(EntryKind::System) => matches!(
                self.subtype.as_deref(),
                Some("turn_duration" | "compact_boundary" | "local_command")
            ),
            _ => false,
        }
    }

    /// A `user` line that ends the turn with no reply to come. See
    /// [`LastEntry::ends_turn`] for the shapes, and [`is_turn_ender`] for the
    /// exact markers.
    fn ends_turn(&self) -> bool {
        if !self.is(EntryKind::User) {
            return false;
        }
        let Some(message) = self.message.as_ref() else {
            return false;
        };
        match &message.content {
            Content::Text(text) => is_turn_ender(text),
            Content::Blocks(blocks) => blocks.iter().any(|block| match block {
                ContentBlock::Raw(text) | ContentBlock::Text(text) => is_turn_ender(text),
                ContentBlock::ToolResult(Some(text)) => is_turn_ender(text),
                _ => false,
            }),
            Content::Absent => false,
        }
    }

    /// The projection the status machine and the activity label consume. Keeping
    /// only this much means the daemon can hold one per session without pinning
    /// whole message bodies in memory.
    pub fn last_entry(&self) -> LastEntry {
        LastEntry {
            kind: self.kind.clone().unwrap_or_default(),
            subtype: self.subtype.clone(),
            role: self.message.as_ref().and_then(|m| m.role),
            has_message: self.message.is_some(),
            tool_uses: self.tool_use_names(),
            has_tool_result: self.has_tool_result(),
            progress: self.data.clone(),
            ends_turn: self.ends_turn(),
            // The accumulator fills this in: it is a property of every line
            // folded so far, not of this one alone.
            activity_at: None,
        }
    }
}
