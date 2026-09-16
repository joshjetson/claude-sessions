//! Ported from the Node app's `test/parser.test.js`, which pins Claude Code's
//! undocumented transcript format. A failure here is either a real regression or
//! the format having moved — the fixtures are the tripwire either way.

mod conversation;
mod cursor;
mod invariants;
mod prompts;
mod task_ref;
mod tool_use;

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

/// A transcript written to a fresh temp directory, kept alive by the returned
/// guard. Tests build their own files rather than sharing one, so they can run
/// in parallel.
pub(crate) struct Transcript {
    _dir: TempDir,
    pub(crate) path: PathBuf,
}

impl Transcript {
    pub(crate) fn new(body: &str) -> Transcript {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, body).expect("write transcript");
        Transcript { _dir: dir, path }
    }

    /// Append without rewriting — what a live session does.
    pub(crate) fn append(&self, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("open for append");
        f.write_all(text.as_bytes()).expect("append");
    }

    pub(crate) fn overwrite(&self, body: &str) {
        std::fs::write(&self.path, body).expect("overwrite");
    }
}

/// One JSONL line.
pub(crate) fn line(json: serde_json::Value) -> String {
    format!("{json}\n")
}

pub(crate) fn user_prompt(text: &str) -> String {
    user_prompt_at(text, "2026-08-01T10:00:00.000Z", serde_json::json!({}))
}

/// A human prompt with extra top-level keys merged in — the sidechain and
/// tool-result cases differ from a real prompt only by one of those.
pub(crate) fn user_prompt_at(text: &str, ts: &str, extra: serde_json::Value) -> String {
    let mut entry = serde_json::json!({
        "type": "user", "userType": "external", "timestamp": ts, "sessionId": "sess-1",
        "message": { "role": "user", "content": text },
    });
    merge(&mut entry, extra);
    line(entry)
}

pub(crate) fn assistant_text(text: &str) -> String {
    assistant_text_with_usage(text, serde_json::json!(null))
}

pub(crate) fn assistant_text_with_usage(text: &str, usage: serde_json::Value) -> String {
    let mut message = serde_json::json!({
        "role": "assistant", "content": [{ "type": "text", "text": text }],
    });
    if !usage.is_null() {
        merge(&mut message, serde_json::json!({ "usage": usage }));
    }
    line(serde_json::json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:01.000Z", "sessionId": "sess-1",
        "message": message,
    }))
}

pub(crate) fn assistant_tool(name: &str, input: serde_json::Value) -> String {
    line(serde_json::json!({
        "type": "assistant", "timestamp": "2026-08-01T10:00:02.000Z", "sessionId": "sess-1",
        "message": { "role": "assistant", "content": [
            { "type": "tool_use", "name": name, "input": input },
        ] },
    }))
}

fn merge(target: &mut serde_json::Value, extra: serde_json::Value) {
    let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (k, v) in extra {
        target.insert(k.clone(), v.clone());
    }
}
