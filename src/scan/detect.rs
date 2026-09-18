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

/// Interpreters that run somebody else's program, so `ps -o comm` reports the
/// interpreter and not the thing a person started.
///
/// This is the npm and bun install of Claude Code: the `claude` on `PATH` is a
/// script with a `#!/usr/bin/env node` line, the kernel execs `node`, and every
/// version of this tool that matched on `comm` alone saw `node` and reported no
/// sessions on a machine that had several. `debian` spells it `nodejs`.
const SCRIPT_RUNTIMES: [&str; 4] = ["node", "nodejs", "bun", "deno"];

/// The last path segment of a `ps` command name, without a Windows extension.
///
/// `comm` is a bare name on Linux and the full executable path on macOS, so the
/// comparison has to be on the file name either way.
fn program_name(comm: &str) -> &str {
    let name = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    name.strip_suffix(".exe").unwrap_or(name)
}

/// Whether a `ps` command name is only a script runtime, and so says nothing
/// about what the process actually is.
pub fn is_script_runtime(comm: &str) -> bool {
    let name = program_name(comm);
    SCRIPT_RUNTIMES
        .iter()
        .any(|runtime| name.eq_ignore_ascii_case(runtime))
}

/// The same judgement as [`is_interactive_claude`], made from the full command
/// line instead of the command name.
///
/// Used for — and only for — a process whose `comm` is a [script
/// runtime](is_script_runtime). The rules are deliberately the identical ones:
/// the deny-list applies to the argv string exactly as it applies to a command
/// name, plus the flag spelling of the same helpers, which is invisible to
/// `comm` and so has always been read from argv.
///
/// It is a deliberate improvement over the Node original, which had no answer
/// for a script install at all. The looseness is the intended direction: a
/// command line that merely mentions `claude` becoming a row somebody asks
/// about is a better failure than a real session being invisible, which is the
/// same trade the comm deny-list already makes.
pub fn argv_is_interactive_claude(argv: &str) -> bool {
    is_interactive_claude(argv) && !is_helper_flag(argv)
}

/// The directory name Claude Code gives a prewarmed worker: `cc-daemon-<n>`.
const SCRATCH_DIR: &str = "cc-daemon-";

/// Where that directory lives, where the platform puts it somewhere fixed.
///
/// Unix: `/tmp`, which is the whole of Node's
/// `/\/(private\/)?tmp\/cc-daemon-\d+\//` — on macOS `/private/tmp/...` contains
/// `/tmp/...` as a substring, so one search answers both spellings.
///
/// Windows: nowhere fixed. The same directory is created under whatever `%TEMP%`
/// points at (`C:\Users\dev\AppData\Local\Temp\cc-daemon-7\` by default), and
/// `%TEMP%` is redirected per user, per session and by every CI runner, so
/// anchoring to a spelling would let a worker through on the machines that moved
/// it. The segment carries the evidence on its own: nothing a person checks out
/// is called `cc-daemon-<digits>`.
const SCRATCH_PARENT: Option<&str> = if cfg!(windows) { None } else { Some("/tmp") };

/// Claude Code keeps its prewarmed workers under a `cc-daemon-<n>` scratch
/// directory; nothing there is a session anybody started.
pub fn is_daemon_scratch_cwd(cwd: &str) -> bool {
    scratch_cwd(cwd, SCRATCH_PARENT, crate::util::SEPARATORS)
}

/// The rule itself, with the platform's two facts passed in — the Windows shape
/// is then a test on every platform rather than only on the one that has it.
pub(super) fn scratch_cwd(cwd: &str, parent: Option<&str>, separators: &[char]) -> bool {
    for (i, _) in cwd.match_indices(SCRATCH_DIR) {
        // A whole path segment, not a prefix of a directory somebody named.
        let before = &cwd[..i];
        if !before.ends_with(separators) {
            continue;
        }
        if parent.is_some_and(|parent| !before.trim_end_matches(separators).ends_with(parent)) {
            continue;
        }
        let rest = &cwd[i + SCRATCH_DIR.len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && rest[digits..].starts_with(separators) {
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

/// The QA run a coordinator was launched for, exported into its environment by
/// the spawn helpers.
///
/// `/\bCLAUDE_SESSIONS_RUN_ID=(\S+)/`, read from the same `ps -E` line as
/// [`launch_task_id`] and for the same reason.
///
/// This exists because the alternative was a guess. A coordinator holds no task
/// id, so it used to be recognised as "a new session, in the launch folder,
/// holding no task" — which the run's own QA sessions also satisfy for the
/// moment between writing a transcript and that transcript being read for a
/// task URL. They all start in the same folder seconds apart, so the match was
/// a race, and losing it meant typing every question into a QA pass that was
/// told not to answer questions. A run id the process carries cannot be raced.
///
/// Unlike a task id this is not numeric — run ids are `project::stage` — so it
/// reads to the end of the value rather than to the end of a digit run. A run
/// id therefore must not contain whitespace, which `id_for` guarantees by
/// construction only insofar as project and stage names do not; a value that
/// does contain a space is truncated here rather than mis-parsed.
pub fn launch_run_id(env_line: &str) -> Option<String> {
    const KEY: &str = "CLAUDE_SESSIONS_RUN_ID=";
    let bytes = env_line.as_bytes();
    for (i, _) in env_line.match_indices(KEY) {
        if !boundary_before(bytes, i) {
            continue;
        }
        let rest = &env_line[i + KEY.len()..];
        // `split`, not `split_whitespace`: the latter skips leading separators,
        // so an EMPTY value (`...RUN_ID= PATH=/usr/bin`) returned the next
        // variable in the line as though it were the run id.
        let value = rest.split(char::is_whitespace).next().unwrap_or_default();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
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
