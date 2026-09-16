//! Discovering the Claude Code skills available on this machine, so a pipeline
//! step can name one without you having to remember what exists.
//!
//! Skills are `SKILL.md` files with YAML-ish frontmatter under:
//!
//! ```text
//! ~/.claude/skills/<name>/SKILL.md              personal
//! ~/.claude/plugins/**/skills/<name>/SKILL.md   installed plugins
//! <repo>/.claude/skills/<name>/SKILL.md         project-local
//! ```
//!
//! The roots are passed in rather than read from `$HOME`, so a test scans a
//! temp tree instead of whatever the person running it happens to have
//! installed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How deep the walk goes below each root. Plugin trees nest a few levels;
/// beyond this is somebody's `node_modules` by another name.
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file: PathBuf,
}

/// The `name` and `description` of a `SKILL.md`.
///
/// Many skills omit `name` entirely — the directory name is the invocation
/// name, which is how Claude Code resolves them — so `fallback_name` is used
/// when the field is absent. Without this, most installed skills are invisible.
/// Returns `None` only when there is no frontmatter *and* no fallback.
pub fn parse_skill_frontmatter(text: &str, fallback_name: &str) -> Option<(String, String)> {
    let Some(body) = frontmatter_body(text) else {
        return (!fallback_name.is_empty()).then(|| (fallback_name.to_string(), String::new()));
    };
    let name = field(body, "name")
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback_name.to_string());
    if name.is_empty() {
        return None;
    }
    Some((name, field(body, "description").unwrap_or_default()))
}

/// Every skill these roots can offer, sorted by name and deduplicated.
///
/// `claude_dir` is `~/.claude`; `repo_path` adds a project's own skills.
pub fn discover_skills(claude_dir: &Path, repo_path: Option<&Path>) -> Vec<Skill> {
    let mut found: HashMap<String, (Skill, SystemTime)> = HashMap::new();
    walk(&claude_dir.join("skills"), 0, &mut found);
    walk(&claude_dir.join("plugins"), 0, &mut found);
    if let Some(repo) = repo_path {
        walk(&repo.join(".claude").join("skills"), 0, &mut found);
    }
    let mut skills: Vec<Skill> = found.into_values().map(|(skill, _)| skill).collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn walk(root: &Path, depth: usize, found: &mut HashMap<String, (Skill, SystemTime)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let skill_file = dir.join("SKILL.md");
        if let Ok(text) = fs::read_to_string(&skill_file) {
            if let Some((name, description)) = parse_skill_frontmatter(&text, &name) {
                // Plugins keep several versions side by side; the newest wins.
                let modified = fs::metadata(&skill_file)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let keep = found
                    .get(&name)
                    .is_none_or(|(_, existing)| modified > *existing);
                if keep {
                    found.insert(
                        name.clone(),
                        (
                            Skill {
                                name,
                                description,
                                file: skill_file,
                            },
                            modified,
                        ),
                    );
                }
            }
        }
        walk(&dir, depth + 1, found);
    }
}

/// The text between the opening and closing `---` lines, if the file starts
/// with a frontmatter block.
fn frontmatter_body(text: &str) -> Option<&str> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest
        .match_indices("\n---")
        .map(|(index, _)| index)
        .find(|index| {
            let after = &rest[index + 4..];
            after.is_empty() || after.starts_with('\n') || after.starts_with('\r')
        })?;
    Some(rest[..end].trim_end_matches('\r'))
}

/// One frontmatter field. Supports `key: value` and the folded `key: >` /
/// `key: |` block forms, which is how long descriptions are usually written.
fn field(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut lines = body.lines().enumerate();
    let (index, line) = lines.find(|(_, line)| line.starts_with(&prefix))?;
    let rest = line[prefix.len()..].trim_start_matches([' ', '\t']);

    let folded = rest.starts_with('>') || rest.starts_with('|');
    if !folded {
        return Some(unquote(rest.trim()));
    }

    // A folded block: the indented lines that follow, joined with spaces.
    let mut collected: Vec<&str> = Vec::new();
    for (_, line) in body.lines().enumerate().skip(index + 1) {
        if line.trim().is_empty() {
            if collected.is_empty() {
                continue;
            }
            break;
        }
        if !line.starts_with([' ', '\t']) {
            break;
        }
        collected.push(line.trim());
    }
    Some(collected.join(" ").trim().to_string())
}

/// Strip one leading and one trailing quote, the way the Node version did —
/// enough for `name: "quoted"` without pretending to be a YAML parser.
fn unquote(text: &str) -> String {
    let trimmed = text.strip_prefix(['"', '\'']).unwrap_or(text);
    trimmed
        .strip_suffix(['"', '\''])
        .unwrap_or(trimmed)
        .to_string()
}
