//! Reading Claude Code's session transcripts.
//!
//! A transcript is a `.jsonl` under `~/.claude/projects/<encoded cwd>/`: one
//! JSON object per line, appended as the session runs. The format is
//! undocumented and moves between releases, so this module is deliberately
//! forgiving — see [`entry`] — and `tests/fixtures/transcripts/` holds eight
//! scrubbed real sessions whose diff is the designed signal that the format
//! changed.
//!
//! Two ways in, sharing one folding core:
//!
//! - **Cold start** — [`parse_session_file`], [`parse_conversation`],
//!   [`parse_session_and_conversation`] read a window off the end of the file.
//!   Use these for a one-off look at a transcript nothing is watching.
//! - **Steady state** — [`TranscriptCursor`] holds a byte offset and folds only
//!   what was appended since the last poll. Anything polling a live session
//!   every tick uses this.

mod cursor;
mod entry;
mod parse;
mod tool_use;

pub use cursor::{CursorPoll, TranscriptCursor};
pub use entry::{extract_text_content, Content, ContentBlock, Entry, Message};
pub use parse::{
    parse_conversation, parse_session_and_conversation, parse_session_file, Collect,
    INITIAL_TAIL_SIZE, MAX_PROMPTS, MAX_TAIL_SIZE,
};
pub use tool_use::{format_tool_use, ToolInput};

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;
