//! The two things both drivers need, written once.
//!
//! The Node app carried a `buildShellCommand` and a `chunkText` in *each*
//! driver. The two `chunkText`s were byte-identical; the two
//! `buildShellCommand`s differed only in whether the `export` clauses were
//! joined into one string before being joined with the rest — which produces
//! the same line either way, so there is nothing here to parameterise. A test
//! pins that equivalence so the claim is checked rather than asserted in prose.

/// Wrap a value in single quotes for `/bin/sh`, escaping any quote it contains.
///
/// POSIX has no escape inside single quotes, so a quote is closed, escaped and
/// reopened: `it's` becomes `'it'\''s'`.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The line a launched window runs: `cd <cwd> && export K='v' && <command>`.
///
/// The environment is exported rather than passed through the driver, because
/// the driver hands the command to a login shell and has no other way to set it.
/// `CLAUDE_SESSIONS_TASK_ID` reaching the child this way is what lets the
/// finished agent report its task back.
pub fn build_shell_command(cwd: &str, command: &str, env: &[(String, String)]) -> String {
    let mut parts = vec![format!("cd {}", shell_quote(cwd))];
    for (key, value) in env {
        parts.push(format!("export {key}={}", shell_quote(value)));
    }
    parts.push(command.to_string());
    parts.join(" && ")
}

/// How much text one write to a terminal may carry.
///
/// A terminal's input queue holds about 1KB (`MAX_INPUT`). Write more than that
/// in one go and the line discipline discards the excess — silently. Revision
/// prompts run to ~1.3KB, so they arrived with their first ~1024 bytes gone and
/// the trailing Return submitted whatever tail was left in the box: agents
/// received "ong assumption, a hidden business rule..." as their entire
/// instruction, one of them for a task belonging to a different project.
/// Chunks are well under the limit, with a pause between them to drain.
pub const SEND_CHUNK_SIZE: usize = 200;

/// Split text into writes no terminal input queue can overrun.
///
/// Chunked by character, not by byte: splitting inside a multi-byte character
/// would send two halves of a glyph, and the Node original had the same hazard
/// with surrogate pairs.
pub fn chunk_text(text: &str, size: usize) -> Vec<String> {
    if text.is_empty() || size == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut count = 0;
    for ch in text.chars() {
        chunk.push(ch);
        count += 1;
        if count == size {
            out.push(std::mem::take(&mut chunk));
            count = 0;
        }
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}
