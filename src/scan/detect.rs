//! What a process's own text says about it: is it a session at all, which
//! transcript does it name, which task was it launched for.
//!
//! Node expressed all of this with five regexes in `src/scanner.js`. They are
//! spelled out by hand here rather than pulling a regex engine into the
//! dependency tree for five fixed patterns; each function quotes the original
//! above it, and the ported Node suite pins every one of them.

/// `[A-Za-z0-9_]` — what `\w` means to the patterns being reproduced.
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `\b` immediately before byte `i`.
fn boundary_before(s: &[u8], i: usize) -> bool {
    i == 0 || !is_word(s[i - 1])
}

/// `\b` immediately after byte `i` (i.e. between `i - 1` and `i`).
fn boundary_after(s: &[u8], i: usize) -> bool {
    i >= s.len() || !is_word(s[i])
}

/// The length of the leading `[\w-]+` run, and whether it contains a word
/// character.
///
/// The trailing `\b` in `bg-[\w-]+\b` is what the second half answers: a run
/// made only of dashes has no boundary to end on, however long it is, so the
/// pattern fails. Any run holding a word character can backtrack to end there.
fn word_dash_run(s: &str) -> (usize, bool) {
    let mut len = 0;
    let mut has_word = false;
    for b in s.bytes() {
        if is_word(b) {
            has_word = true;
        } else if b != b'-' {
            break;
        }
        len += 1;
    }
    (len, has_word)
}

/// Claude Code runs several helper subcommands under the same binary name:
/// prewarmed PTY workers (`bg-spare`, `bg-pty-host`), its background daemon,
/// MCP bridges. They are created and recycled on Claude Code's own schedule, so
/// counting them made sessions appear and vanish in the dashboard with nobody
/// touching anything.
const HELPER_SUBCOMMANDS: [&str; 8] = [
    "daemon",
    "mcp",
    "update",
    "install",
    "doctor",
    "plugin",
    "config",
    "migrate-installer",
];

/// `/\bclaude\s+(bg-[\w-]+|daemon|mcp|update|install|doctor|plugin|config|migrate-installer)\b/`
fn has_helper_subcommand(comm: &str) -> bool {
    let bytes = comm.as_bytes();
    for (i, _) in comm.match_indices("claude") {
        if !boundary_before(bytes, i) {
            continue;
        }
        let after_word = i + "claude".len();
        let mut j = after_word;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j == after_word {
            continue; // `\s+` wants at least one
        }
        let rest = &comm[j..];
        if let Some(tail) = rest.strip_prefix("bg-") {
            if word_dash_run(tail).1 {
                return true;
            }
            continue;
        }
        for sub in HELPER_SUBCOMMANDS {
            if rest.starts_with(sub) && boundary_after(bytes, j + sub.len()) {
                return true;
            }
        }
    }
    false
}

/// Whether a `ps` command name is a session a person is sitting in front of.
///
/// Deliberately a DENY-list. An allow-list would hide a new interactive mode
/// the day Claude Code ships one, and nobody would think to update it; an
/// unfamiliar subcommand showing up in the dashboard is a question somebody
/// asks, which is the outcome worth having.
pub fn is_interactive_claude(comm: &str) -> bool {
    if comm.is_empty() || !comm.contains("claude") {
        return false;
    }
    if comm.contains("claude-sessions") {
        return false; // this tool itself
    }
    !has_helper_subcommand(comm)
}

/// Claude Code keeps its prewarmed workers under `/tmp/cc-daemon-<n>/`; nothing
/// there is a session anybody started.
///
/// `/\/(private\/)?tmp\/cc-daemon-\d+\//` — on macOS `/private/tmp/…` contains
/// `/tmp/…` as a substring, so one search answers both spellings.
pub fn is_daemon_scratch_cwd(cwd: &str) -> bool {
    const MARKER: &str = "/tmp/cc-daemon-";
    for (i, _) in cwd.match_indices(MARKER) {
        let rest = &cwd[i + MARKER.len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && rest[digits..].starts_with('/') {
            return true;
        }
    }
    false
}

/// A session id is a 36-character UUID.
const SESSION_ID_LEN: usize = 36;

/// The transcript a process names on its own command line, if it names one.
///
/// `/--(?:resume|session-id)[= ]([0-9a-fA-F-]{36})/` — `--resume` for a
/// transcript it is continuing, `--session-id` for one it is about to create.
/// Either is proof of which file the process owns, and nothing outranks it.
///
/// Read from argv ONLY, never from the environment: see [`launch_task_id`].
pub fn session_id_flag(argv: &str) -> Option<String> {
    for i in 0..argv.len() {
        if !argv.is_char_boundary(i) {
            continue;
        }
        let rest = &argv[i..];
        let Some(after) = rest
            .strip_prefix("--resume")
            .or_else(|| rest.strip_prefix("--session-id"))
        else {
            continue;
        };
        let value = match after.as_bytes().first() {
            Some(b'=') | Some(b' ') => &after[1..],
            _ => continue,
        };
        if value.len() < SESSION_ID_LEN {
            continue;
        }
        let id = &value[..SESSION_ID_LEN];
        if id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Some(id.to_string());
        }
    }
    None
}

/// The helper subcommands [`is_interactive_claude`] catches, in their flag
/// spelling (`claude --bg-pty-host`).
///
/// `/\s--bg-[\w-]+\b/`. That form is invisible to `ps -o comm`, so a PTY host
/// showed up as a session and competed for a transcript with the real one.
pub fn is_helper_flag(argv: &str) -> bool {
    const MARKER: &str = "--bg-";
    let bytes = argv.as_bytes();
    for (i, _) in argv.match_indices(MARKER) {
        if i == 0 || !bytes[i - 1].is_ascii_whitespace() {
            continue;
        }
        if word_dash_run(&argv[i + MARKER.len()..]).1 {
            return true;
        }
    }
    false
}

/// The task a session was launched for, exported into its environment by the
/// spawn helpers.
///
/// `/\bCLAUDE_SESSIONS_TASK_ID=(\d+)/`. Read from a SEPARATE `ps -E` call to
/// the one that reads argv, so that no environment value can be mistaken for a
/// command-line flag: a prompt or a path sitting in the environment must never
/// look like `--resume`.
pub fn launch_task_id(env_line: &str) -> Option<i64> {
    const KEY: &str = "CLAUDE_SESSIONS_TASK_ID=";
    let bytes = env_line.as_bytes();
    for (i, _) in env_line.match_indices(KEY) {
        if !boundary_before(bytes, i) {
            continue;
        }
        let rest = &env_line[i + KEY.len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            return rest[..digits].parse().ok();
        }
    }
    None
}
