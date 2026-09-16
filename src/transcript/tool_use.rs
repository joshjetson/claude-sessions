//! Rendering a `tool_use` block as the one line (or small diff) the
//! conversation pane shows.
//!
//! Ported arm for arm from `parser.ts`'s `formatToolUse`; `test/parser.test.js`
//! pins every rendering here, so the strings are contracts rather than taste.

use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// A tool call's arguments, **in the order the transcript wrote them**.
///
/// A plain `serde_json::Map` would sort the keys, and the fallback arm below
/// renders the first string parameter — with sorted keys an
/// `mcp__…__execute_method(model, method, args)` call would advertise the wrong
/// one. Lookups are a linear scan of a handful of keys, which is cheaper than
/// the hashing a map would do anyway.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolInput(Vec<(String, Value)>);

impl ToolInput {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// A parameter that is a non-empty string — which is exactly what Node's
    /// `inp.foo ? … : …` truthiness tests accepted.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::String(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    fn first_str(&self) -> Option<&str> {
        self.0.iter().find_map(|(_, v)| match v {
            Value::String(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for ToolInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedPairs;

        impl<'de> Visitor<'de> for OrderedPairs {
            type Value = ToolInput;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a tool input object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<ToolInput, M::Error> {
                let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(4));
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    pairs.push((k, v));
                }
                Ok(ToolInput(pairs))
            }
        }

        deserializer.deserialize_map(OrderedPairs)
    }
}

/// Node's `shortPath`: the last two `/`-separated pieces, leading empty piece
/// included, so `/a/b.js` reads `a/b.js` and a bare `b.js` stays `b.js`.
fn short_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = path.split('/').collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

/// `s`, or its first `keep` characters with an ellipsis appended.
///
/// Node sliced UTF-16 code units; this counts characters, which differs only on
/// non-ASCII arguments and cannot split one in half.
fn clip(s: &str, max: usize, keep: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(keep).collect();
        out.push_str("...");
        out
    } else {
        s.to_string()
    }
}

/// One line describing a tool call, or — for `Edit` — a heading plus a `-`/`+`
/// diff body.
pub fn format_tool_use(name: &str, input: &ToolInput) -> String {
    let path_of = |key: &str| short_path(input.str(key).unwrap_or(""));
    // `Glob`/`Grep` append " in <dir>" only when a path was given.
    let in_path = || match input.str("path") {
        Some(p) => format!(" in {}", short_path(p)),
        None => String::new(),
    };

    match name {
        "Read" => format!("[Read: {}]", path_of("file_path")),
        "Write" => format!("[Write: {}]", path_of("file_path")),
        "Edit" => {
            let mut lines = vec![format!("[Edit: {}]", path_of("file_path"))];
            if let Some(old) = input.str("old_string") {
                lines.extend(old.split('\n').map(|l| format!("- {l}")));
            }
            if let Some(new) = input.str("new_string") {
                lines.extend(new.split('\n').map(|l| format!("+ {l}")));
            }
            lines.join("\n")
        }
        // 80 characters, because a pasted heredoc would otherwise take the pane.
        "Bash" => match input.str("command") {
            Some(cmd) => format!("[Bash: {}]", clip(cmd, 80, 77)),
            None => "[Bash]".to_string(),
        },
        "Glob" => format!(
            "[Glob: {}{}]",
            input.str("pattern").unwrap_or(""),
            in_path()
        ),
        "Grep" => format!(
            "[Grep: \"{}\"{}]",
            input.str("pattern").unwrap_or(""),
            in_path()
        ),
        "WebSearch" => format!("[Search: \"{}\"]", input.str("query").unwrap_or("")),
        "WebFetch" => format!("[Fetch: {}]", input.str("url").unwrap_or("")),
        "Task" => format!(
            "[Task: {}]",
            input
                .str("description")
                .or_else(|| input.str("subagent_type"))
                .unwrap_or("agent")
        ),
        "TodoWrite" => "[TodoWrite]".to_string(),
        "AskUserQuestion" => match first_question(input) {
            // Note the 60/60 split here against 80/77 above and 60/57 below —
            // Node used all three, and the tests pin each.
            Some(q) => format!("[Question: {}]", clip(q, 60, 60)),
            None => "[AskUserQuestion]".to_string(),
        },
        // Everything else, MCP tools included: the name plus its first string
        // argument. Deliberately open-ended — a tool we have never heard of
        // still says something useful.
        _ => match input.first_str() {
            Some(v) => format!("[{name}: {}]", clip(v, 60, 57)),
            None => format!("[{name}]"),
        },
    }
}

fn first_question(input: &ToolInput) -> Option<&str> {
    input
        .get("questions")?
        .as_array()?
        .first()?
        .get("question")?
        .as_str()
        .filter(|q| !q.is_empty())
}
