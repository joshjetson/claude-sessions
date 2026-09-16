//! The incremental reader: one open position per transcript, advanced by
//! exactly the bytes that were appended.
//!
//! This is the headline reason the port exists. Node re-read the last 512 KB of
//! every live transcript and re-parsed it from scratch once a second — with
//! eight sessions open that is 4 MB read and ~4 MB of JSON parsed per tick, to
//! learn about the handful of lines that actually arrived. A cursor keeps the
//! byte offset it stopped at and the fragment of a half-written line it was
//! holding, so a poll costs the size of the append.
//!
//! The cold-start path ([`super::parse_session_file`] and friends) and this one
//! fold their lines through the same [`Accumulator`], so there is one set of
//! rules for what a transcript means.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::parse::{cold_parse, feed_lines, file_id, Accumulator, Collect, Window};
use crate::types::{ConversationMessage, LastEntry, ParsedSession, SessionMetaLite};

/// What one [`TranscriptCursor::poll`] did, for the daemon's bookkeeping and
/// for the tests that hold this module to its O(appended-bytes) promise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorPoll {
    /// Bytes read from the file this poll. Zero when nothing was appended.
    pub bytes_read: u64,
    /// Complete lines folded this poll.
    pub lines: usize,
    /// The file was truncated or replaced, so the window was re-read in full.
    pub reloaded: bool,
}

impl CursorPoll {
    /// Whether anything about the session changed — what a refresh uses to
    /// decide whether a row needs re-rendering.
    pub fn changed(&self) -> bool {
        self.lines > 0 || self.reloaded
    }
}

/// A transcript file plus the position, carry-over fragment and folded state
/// that make re-reading it unnecessary.
///
/// Create one per live session; hold it for as long as the session lives.
#[derive(Debug)]
pub struct TranscriptCursor {
    path: PathBuf,
    collect: Collect,
    /// One past the last byte folded.
    offset: u64,
    /// A line that arrived without its newline yet.
    carry: Vec<u8>,
    /// Inode, when the platform has one: a transcript restored from an archive
    /// keeps its path and its length but is a different file.
    file_id: Option<u64>,
    acc: Accumulator,
    bytes_read: u64,
}

impl TranscriptCursor {
    /// Open a transcript and fold its tail — the cold start. Afterwards
    /// [`poll`](Self::poll) only ever reads appended bytes.
    ///
    /// `collect` decides whether the conversation is built as well; a cursor
    /// created for the sessions list carries only session state, so the daemon
    /// does not hold every message of every open session in memory.
    pub fn open(path: impl Into<PathBuf>, collect: Collect) -> io::Result<Self> {
        let path = path.into();
        let parsed = cold_parse(&path, collect, Window::GrowUntilPrompts)?;
        Ok(TranscriptCursor {
            bytes_read: parsed.end,
            offset: parsed.end,
            carry: parsed.carry,
            file_id: parsed.file_id,
            acc: parsed.acc,
            collect,
            path,
        })
    }

    /// Fold whatever has been appended since the last poll.
    ///
    /// Reads `size - offset` bytes and nothing else. A file that shrank (a
    /// `> file` truncation) or that has a new inode (an archive restored over
    /// it) cannot be continued from an offset, so the tail is re-read and the
    /// folded state replaced — `reloaded` says so.
    pub fn poll(&mut self) -> io::Result<CursorPoll> {
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let size = meta.len();
        let id = file_id(&meta);

        if size < self.offset || id != self.file_id {
            return self.reload();
        }
        if size == self.offset {
            return Ok(CursorPoll::default());
        }

        let want = size - self.offset;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut appended = Vec::with_capacity(want as usize);
        file.take(want).read_to_end(&mut appended)?;
        let read = appended.len() as u64;

        // The carry is usually empty, so the common poll parses the appended
        // bytes where they landed.
        let mut buf = std::mem::take(&mut self.carry);
        if buf.is_empty() {
            buf = appended;
        } else {
            buf.extend_from_slice(&appended);
        }

        let mut lines = 0usize;
        let acc = &mut self.acc;
        let carry = feed_lines(&buf, |line| {
            if acc.push_line(line) {
                lines += 1;
            }
        });
        self.carry = carry.to_vec();
        self.offset += read;
        self.bytes_read += read;

        Ok(CursorPoll {
            bytes_read: read,
            lines,
            reloaded: false,
        })
    }

    fn reload(&mut self) -> io::Result<CursorPoll> {
        let parsed = cold_parse(&self.path, self.collect, Window::GrowUntilPrompts)?;
        self.offset = parsed.end;
        self.carry = parsed.carry;
        self.file_id = parsed.file_id;
        self.acc = parsed.acc;
        self.bytes_read += parsed.end;
        Ok(CursorPoll {
            bytes_read: parsed.end,
            lines: 0,
            reloaded: true,
        })
    }

    /// Everything a session row needs, as of the last poll.
    pub fn session(&self) -> ParsedSession {
        self.acc.parsed_session()
    }

    /// The cheap half of [`session`](Self::session).
    pub fn meta(&self) -> SessionMetaLite {
        self.acc.meta()
    }

    /// The trailing entry, borrowed — what the status machine takes.
    pub fn last_entry(&self) -> Option<&LastEntry> {
        self.acc.last_entry()
    }

    /// The conversation so far; empty unless the cursor was opened with
    /// [`Collect::SessionAndConversation`].
    pub fn messages(&self) -> &[ConversationMessage] {
        self.acc.messages()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes this cursor has read since it was opened, cold start included.
    /// Exposed so the O(delta) promise can be asserted rather than believed.
    pub fn total_bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
