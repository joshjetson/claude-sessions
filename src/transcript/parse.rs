//! Turning a transcript file into a [`ParsedSession`] and the conversation the
//! pane renders.
//!
//! Two paths share one core. [`Accumulator`] is fed complete lines and knows
//! nothing about files; the functions here are the **cold start** (read a window
//! off the end of the file and fold it), and [`super::TranscriptCursor`] is the
//! **steady state** (fold only the bytes appended since the last poll). Node had
//! neither split — it re-read and re-parsed 512 KB per active session per
//! second.

use std::collections::VecDeque;
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::entry::{Content, ContentBlock, Entry};
use super::tool_use::format_tool_use;
use crate::types::{
    ConversationMessage, CumulativeUsage, LastEntry, MessageRole, ParsedSession, Prompt,
    SessionMetaLite, Usage,
};

/// First window read back from the end of a transcript.
pub const INITIAL_TAIL_SIZE: u64 = 512 * 1024;
/// Hard ceiling on the window, doubled into from [`INITIAL_TAIL_SIZE`].
pub const MAX_TAIL_SIZE: u64 = 4 * 1024 * 1024;
/// How many recent human prompts a session keeps.
pub const MAX_PROMPTS: usize = 5;

/// What a caller wants out of a fold. The conversation is a strict superset, so
/// this is one parameter rather than two near-identical parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collect {
    /// Session meta, prompts and usage — what a session row needs.
    Session,
    /// The same, plus the rendered turns for the conversation pane.
    SessionAndConversation,
}

/// Everything a run of transcript lines adds up to.
///
/// One instance folds a cold-start window and then, inside a cursor, every
/// appended line for the life of the session — so the rules below are written
/// exactly once.
#[derive(Debug)]
pub(crate) struct Accumulator {
    collect: Collect,
    session_id: String,
    git_branch: String,
    last_timestamp: String,
    last_usage: Option<Usage>,
    last_entry: Option<LastEntry>,
    cumulative: CumulativeUsage,
    /// Capped as it grows; Node pushed every prompt and sliced at the end.
    prompts: VecDeque<Prompt>,
    messages: Vec<ConversationMessage>,
}

impl Accumulator {
    pub(crate) fn new(collect: Collect) -> Self {
        Accumulator {
            collect,
            session_id: String::new(),
            git_branch: String::new(),
            last_timestamp: String::new(),
            last_usage: None,
            last_entry: None,
            cumulative: CumulativeUsage::default(),
            prompts: VecDeque::with_capacity(MAX_PROMPTS),
            messages: Vec::new(),
        }
    }

    /// Fold one line. Returns whether it was JSON we could read; unparseable
    /// lines are skipped, never an error.
    pub(crate) fn push_line(&mut self, line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line);
        match Entry::parse_line(&text) {
            Some(entry) => {
                self.push_entry(&entry);
                true
            }
            None => false,
        }
    }

    fn push_entry(&mut self, entry: &Entry) {
        // Every entry updates the trailing projection, including the bookkeeping
        // types the rest of this function ignores: the status machine reads the
        // *last* line of the file, whatever it happens to be.
        self.last_entry = Some(entry.last_entry());

        // The first id wins (a transcript names itself once); the newest branch
        // and timestamp win (both move mid-session).
        if self.session_id.is_empty() {
            if let Some(id) = entry.session_id() {
                self.session_id = id.to_string();
            }
        }
        if let Some(branch) = entry.git_branch() {
            self.git_branch = branch.to_string();
        }
        if !entry.timestamp().is_empty() {
            self.last_timestamp = entry.timestamp().to_string();
        }

        if entry.message.is_none() {
            return;
        }

        if let Some(text) = entry.human_prompt() {
            if self.prompts.len() == MAX_PROMPTS {
                self.prompts.pop_front();
            }
            self.prompts.push_back(Prompt {
                text: text.to_string(),
                timestamp: entry.timestamp().to_string(),
            });
            if self.collect == Collect::SessionAndConversation {
                self.messages.push(ConversationMessage {
                    role: MessageRole::User,
                    text: text.to_string(),
                    timestamp: entry.timestamp().to_string(),
                    has_tool_use: false,
                    usage: None,
                });
            }
        }

        if let Some(usage) = entry.assistant_usage() {
            self.last_usage = Some(*usage);
            self.cumulative.input_tokens += usage.input_tokens.unwrap_or(0);
            self.cumulative.cache_creation_input_tokens +=
                usage.cache_creation_input_tokens.unwrap_or(0);
            self.cumulative.cache_read_input_tokens += usage.cache_read_input_tokens.unwrap_or(0);
            self.cumulative.output_tokens += usage.output_tokens.unwrap_or(0);
        }

        if self.collect == Collect::SessionAndConversation {
            self.push_assistant_turn(entry);
        }
    }

    /// An assistant turn as one block of text: prose and tool calls in the order
    /// they were emitted. Node dropped turns that rendered to nothing, so a
    /// thinking-only turn leaves no row.
    fn push_assistant_turn(&mut self, entry: &Entry) {
        let Some(message) = entry.assistant_message() else {
            return;
        };
        let mut parts: Vec<String> = Vec::new();
        let mut has_tool_use = false;
        match &message.content {
            Content::Text(s) => parts.push(s.clone()),
            Content::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text(t) if !t.is_empty() => parts.push(t.clone()),
                        ContentBlock::ToolUse {
                            name: Some(name),
                            input,
                        } => {
                            has_tool_use = true;
                            parts.push(format_tool_use(name, input));
                        }
                        _ => {}
                    }
                }
            }
            Content::Absent => {}
        }
        let text = parts.join("\n");
        if text.is_empty() {
            return;
        }
        self.messages.push(ConversationMessage {
            role: MessageRole::Assistant,
            text,
            timestamp: entry.timestamp().to_string(),
            has_tool_use,
            usage: message.usage,
        });
    }

    pub(crate) fn has_prompts(&self) -> bool {
        !self.prompts.is_empty()
    }

    pub(crate) fn last_entry(&self) -> Option<&LastEntry> {
        self.last_entry.as_ref()
    }

    pub(crate) fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    pub(crate) fn parsed_session(&self) -> ParsedSession {
        ParsedSession {
            session_id: self.session_id.clone(),
            git_branch: self.git_branch.clone(),
            last_timestamp: self.last_timestamp.clone(),
            last_usage: self.last_usage,
            last_entry: self.last_entry.clone(),
            cumulative_usage: self.cumulative,
            prompts: self.prompts.iter().cloned().collect(),
        }
    }

    pub(crate) fn meta(&self) -> SessionMetaLite {
        SessionMetaLite {
            session_id: self.session_id.clone(),
            last_timestamp: self.last_timestamp.clone(),
            last_usage: self.last_usage,
            last_entry: self.last_entry.clone(),
        }
    }

    pub(crate) fn into_messages(self) -> Vec<ConversationMessage> {
        self.messages
    }
}

/// Feed every newline-terminated line in `buf` to `f`, and return the trailing
/// bytes that have no newline yet.
pub(crate) fn feed_lines(buf: &[u8], mut f: impl FnMut(&[u8])) -> &[u8] {
    let mut rest = buf;
    while let Some(end) = rest.iter().position(|b| *b == b'\n') {
        f(&rest[..end]);
        rest = &rest[end + 1..];
    }
    rest
}

/// How much of the file to read back from its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Window {
    /// One [`INITIAL_TAIL_SIZE`] read, however it turns out.
    Initial,
    /// Double the window (to [`MAX_TAIL_SIZE`]) until a human prompt is in it.
    /// A session row without a prompt has nothing to show, and the prompt can
    /// sit far behind a long run of tool output.
    GrowUntilPrompts,
}

/// The result of reading and folding a window off the end of a file, with
/// everything a cursor needs to carry on from where it stopped.
pub(crate) struct ColdParse {
    pub(crate) acc: Accumulator,
    /// One past the last byte folded — where an incremental read resumes.
    pub(crate) end: u64,
    /// Trailing bytes with no newline yet: a line still being written.
    pub(crate) carry: Vec<u8>,
    pub(crate) file_id: Option<u64>,
    /// True when the window reached byte 0, so there is nothing more to find.
    from_start: bool,
}

impl ColdParse {
    /// Fold the trailing partial line as well.
    ///
    /// The one-shot parsers do this because Node did: it split the whole window
    /// on newlines and tried every piece, so a file whose last line has no
    /// terminator still contributed it. A cursor must *not* — it holds those
    /// bytes back until the rest of the line arrives.
    pub(crate) fn including_trailing_line(mut self) -> Accumulator {
        self.acc.push_line(&self.carry);
        self.acc
    }
}

#[cfg(unix)]
pub(crate) fn file_id(meta: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
pub(crate) fn file_id(_meta: &Metadata) -> Option<u64> {
    None
}

/// Read a window off the end of `path` and fold it.
pub(crate) fn cold_parse(path: &Path, collect: Collect, window: Window) -> io::Result<ColdParse> {
    let mut size = INITIAL_TAIL_SIZE;
    loop {
        let parsed = read_and_fold(path, collect, size)?;
        // Growing is pointless once the window already reaches byte 0 — Node
        // kept doubling and re-parsing the same bytes up to four times for every
        // transcript with no prompt in it.
        if window == Window::Initial
            || parsed.acc.has_prompts()
            || parsed.from_start
            || size >= MAX_TAIL_SIZE
        {
            return Ok(parsed);
        }
        size *= 2;
    }
}

fn read_and_fold(path: &Path, collect: Collect, window: u64) -> io::Result<ColdParse> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let file_size = meta.len();
    let read_size = window.min(file_size);
    let offset = file_size - read_size;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(read_size as usize);
    file.take(read_size).read_to_end(&mut bytes)?;

    // A window that starts mid-file starts mid-line; that fragment is not JSON
    // and must not be handed to the parser as if it were a whole entry. (When
    // the window holds no newline at all there is nothing to trim — the single
    // fragment simply fails to parse, as it did in Node.)
    let mut trimmed: &[u8] = &bytes;
    if offset > 0 {
        if let Some(first) = trimmed.iter().position(|b| *b == b'\n') {
            trimmed = &trimmed[first + 1..];
        }
    }

    let mut acc = Accumulator::new(collect);
    let carry = feed_lines(trimmed, |line| {
        acc.push_line(line);
    });

    Ok(ColdParse {
        acc,
        end: offset + bytes.len() as u64,
        carry: carry.to_vec(),
        file_id: file_id(&meta),
        from_start: offset == 0,
    })
}

/// Everything a session row and the alerting need from a transcript.
///
/// Reads the tail only, growing the window until it holds a human prompt. An
/// unreadable *line* is skipped; only an unreadable *file* is an error.
pub fn parse_session_file(path: &Path) -> io::Result<ParsedSession> {
    let parsed = cold_parse(path, Collect::Session, Window::GrowUntilPrompts)?;
    Ok(parsed.including_trailing_line().parsed_session())
}

/// The turns the conversation pane renders, from one [`INITIAL_TAIL_SIZE`] read.
pub fn parse_conversation(path: &Path) -> io::Result<Vec<ConversationMessage>> {
    let parsed = cold_parse(path, Collect::SessionAndConversation, Window::Initial)?;
    Ok(parsed.including_trailing_line().into_messages())
}

/// Session meta and conversation from a single read — what the pane's refresh
/// uses, so selecting a session does not read its transcript twice.
pub fn parse_session_and_conversation(
    path: &Path,
) -> io::Result<(SessionMetaLite, Vec<ConversationMessage>)> {
    let parsed = cold_parse(path, Collect::SessionAndConversation, Window::Initial)?;
    let acc = parsed.including_trailing_line();
    Ok((acc.meta(), acc.into_messages()))
}
