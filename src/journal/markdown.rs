//! The small markdown primitives the journal parsers share.
//!
//! Hand-rolled rather than pulled from a crate: the journal only needs line
//! prefixes, paragraphs and bold labels, and a markdown dependency would be a
//! large addition to a `cargo install` for four helpers.

// --- small markdown helpers -------------------------------------------------

/// Does any line start with `prefix` followed by a space or tab?
pub(super) fn has_line_prefix(body: &str, prefix: &str) -> bool {
    body.split('\n')
        .any(|line| line_prefix_len(line, prefix).is_some())
}

/// `prefix` followed by at least one space or tab, at the start of `line`.
pub(super) fn line_prefix_len(line: &str, prefix: &str) -> Option<usize> {
    let rest = line.strip_prefix(prefix)?;
    let trimmed = rest.trim_start_matches([' ', '\t']);
    (trimmed.len() < rest.len()).then(|| prefix.len() + (rest.len() - trimmed.len()))
}

/// Split at every line starting with `prefix`, dropping what came before the
/// first one. `split_keeping_preamble` keeps it as element 0.
pub(super) fn split_at_prefix<'a>(body: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut parts = split_keeping_preamble(body, prefix);
    parts.remove(0);
    parts
}

pub(super) fn split_keeping_preamble<'a>(body: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if let Some(used) = line_prefix_len(line, prefix) {
            parts.push(&body[start..offset]);
            start = offset + used;
        }
        offset += line.len();
    }
    parts.push(&body[start..]);
    parts
}

/// Everything after the first newline, and the line before it.
pub(super) fn split_first_line(block: &str) -> (&str, &str) {
    match block.find('\n') {
        Some(at) => (&block[..at], &block[at + 1..]),
        None => (block, ""),
    }
}

/// `\n{2,}` — paragraphs.
pub(super) fn split_paragraphs(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            let mut end = index + 1;
            let mut count = 1;
            while end < bytes.len() && bytes[end] == b'\n' {
                count += 1;
                end += 1;
            }
            if count >= 2 {
                out.push(&body[start..index]);
                start = end;
                index = end;
                continue;
            }
            index = end;
            continue;
        }
        index += 1;
    }
    out.push(&body[start..]);
    out
}

pub(super) fn markdown_heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    (trimmed.len() < rest.len()).then(|| trimmed.trim_end())
}

pub(super) fn markdown_bullet(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix(['-', '*'])?;
    let trimmed = rest.trim_start_matches([' ', '\t']);
    (trimmed.len() < rest.len()).then(|| trimmed.trim_end())
}
