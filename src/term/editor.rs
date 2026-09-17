//! Handing the terminal to `$EDITOR` and taking it back.
//!
//! The dashboard owns the terminal — alternate screen, raw mode, its own key
//! reader — so an editor cannot simply be spawned into it: both would paint over
//! each other. Leaving and restoring the alternate screen is the UI layer's job;
//! this module is the part in the middle, which is why [`run_editor`] is a plain
//! function the TUI calls inside its suspend closure rather than something that
//! knows about a terminal guard.

use std::env;
use std::path::Path;
use std::process::Command;

use super::spawn::SpawnPolicy;

/// Editors that take `+N` to open at a line.
const PLUS_LINE: [&str; 10] = [
    "vim",
    "nvim",
    "vi",
    "view",
    "nano",
    "pico",
    "emacs",
    "emacsclient",
    "kak",
    "joe",
];
/// Editors that take `--goto file:line`.
const GOTO: [&str; 5] = ["code", "code-insiders", "cursor", "windsurf", "codium"];
/// Editors that take a bare `file:line`.
const SUFFIX_LINE: [&str; 2] = ["subl", "sublime_text"];

const GOTO_FLAG: &str = "--goto";
const FALLBACK: &str = "vi";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorSource {
    Visual,
    Editor,
    Default,
}

/// The editor to run, already split into a program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub cmd: String,
    pub args: Vec<String>,
    pub source: EditorSource,
}

impl Editor {
    /// For callers that already know exactly what to run.
    pub fn command(cmd: impl Into<String>) -> Self {
        Editor {
            cmd: cmd.into(),
            args: Vec::new(),
            source: EditorSource::Default,
        }
    }

    /// The program's name, without its directory and — on Windows — without the
    /// launcher extension. `nvim.exe` and `code.cmd` are the same editors as
    /// `nvim` and `code`, and the per-editor line flags below are matched by
    /// name. A Unix file really can be called `vim.exe`, so nothing is stripped
    /// there.
    fn basename(&self) -> &str {
        let name = Path::new(&self.cmd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.cmd);
        if !cfg!(windows) {
            return name;
        }
        for suffix in [".exe", ".cmd", ".bat", ".com"] {
            if name.len() > suffix.len() && name.to_ascii_lowercase().ends_with(suffix) {
                return &name[..name.len() - suffix.len()];
            }
        }
        name
    }
}

/// Which editor to use. `$VISUAL` wins over `$EDITOR` — that is the convention,
/// `VISUAL` being the full-screen one — then a plain `vi`, which is on every
/// Unix box.
///
/// The value may carry arguments (`code -w`, `subl -n -w`), so it is split;
/// without that a GUI editor returns instantly and the dashboard repaints over
/// a window the user is still typing in.
pub fn resolve_editor(visual: Option<&str>, editor: Option<&str>) -> Editor {
    fn pick(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    let (raw, source) = match (pick(visual), pick(editor)) {
        (Some(visual), _) => (visual, EditorSource::Visual),
        (None, Some(editor)) => (editor, EditorSource::Editor),
        (None, None) => (FALLBACK, EditorSource::Default),
    };
    let mut parts = raw.split_whitespace().map(str::to_string);
    Editor {
        cmd: parts.next().unwrap_or_else(|| FALLBACK.to_string()),
        args: parts.collect(),
        source,
    }
}

/// The same, from this process's environment. Read here rather than threaded
/// through the UI, because `$EDITOR` is genuinely a property of the shell the
/// dashboard was started from.
pub fn resolve_editor_from_env() -> Editor {
    let visual = env::var("VISUAL").ok();
    let editor = env::var("EDITOR").ok();
    resolve_editor(visual.as_deref(), editor.as_deref())
}

/// Arguments that open a file on `line`, in whatever syntax this editor uses.
///
/// Unknown editors just get the filename — opening at the top is fine, opening
/// with a flag they do not understand is not.
pub fn line_args(cmd: &str, line: u32) -> Vec<String> {
    if line == 0 {
        return Vec::new();
    }
    let name = Editor::command(cmd);
    let name = name.basename();
    if PLUS_LINE.contains(&name) {
        return vec![format!("+{line}")];
    }
    if GOTO.contains(&name) {
        return vec![GOTO_FLAG.to_string()];
    }
    Vec::new()
}

/// The full argument vector, pure so the per-editor syntax is tested without
/// starting anything.
pub fn build_editor_argv(editor: &Editor, file: &str, line: u32) -> Vec<String> {
    let extra = line_args(&editor.cmd, line);
    // The VS Code family takes `--goto file:line`; Sublime takes the suffix with
    // no flag at all; everything else takes a flag and then the plain filename.
    let suffixed = line > 0
        && (extra.first().map(String::as_str) == Some(GOTO_FLAG)
            || SUFFIX_LINE.contains(&editor.basename()));
    let target = if suffixed {
        format!("{file}:{line}")
    } else {
        file.to_string()
    };

    let mut argv = editor.args.clone();
    argv.extend(extra);
    argv.push(target);
    argv
}

/// What running the editor did. Never an `Err`: a throw here would leave the UI
/// unmounted and the terminal half-restored, so a missing editor is reported
/// rather than propagated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorOutcome {
    pub ok: bool,
    pub code: Option<i32>,
    pub error: Option<String>,
}

/// Run the editor on a file, optionally at a line, inheriting the terminal so it
/// behaves exactly as it would from a shell.
///
/// `policy` because this is a spawn like any other — see
/// [`SpawnPolicy::check`]. A test that means to run a real editor passes
/// [`SpawnPolicy::Allow`] and says why.
pub fn run_editor(editor: &Editor, file: &str, line: u32, policy: SpawnPolicy) -> EditorOutcome {
    if let Err(refused) = policy.check("open an editor") {
        return EditorOutcome {
            ok: false,
            code: None,
            error: Some(refused.message),
        };
    }

    let argv = build_editor_argv(editor, file, line);
    match Command::new(&editor.cmd).args(&argv).status() {
        Err(err) => EditorOutcome {
            ok: false,
            code: None,
            error: Some(format!("could not start {}: {err}", editor.cmd)),
        },
        Ok(status) if status.success() => EditorOutcome {
            ok: true,
            code: status.code(),
            error: None,
        },
        Ok(status) => EditorOutcome {
            ok: false,
            code: status.code(),
            error: Some(match status.code() {
                Some(code) => format!("{} exited with {code}", editor.cmd),
                None => format!("{} was killed before it exited", editor.cmd),
            }),
        },
    }
}
