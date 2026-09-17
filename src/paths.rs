//! Every filesystem location the tool uses, resolved once and passed down.
//!
//! The Node app read `process.env` at module load in three different files, and
//! the helper CLIs (`notify`, `done`, `blocked`) skipped that machinery entirely
//! and joined onto `homedir()` themselves — so `CLAUDE_SESSIONS_HOME` relocated
//! the dashboard's state but not the markers the agents wrote, and its test
//! suite needed a helper that had to be imported before anything else to patch
//! the environment in time.
//!
//! Here there is one [`Paths`] value, built in `main`, handed to whatever needs
//! it. Tests build their own with [`Paths::for_test`], which makes them
//! parallel-safe without touching the process environment.

use std::env;
use std::path::{Path, PathBuf};

/// Environment variables that move state off its default location. Captured in
/// one place so the resolution rules are readable, and so tests can exercise
/// them by value instead of by mutating the process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathEnv {
    /// `CLAUDE_SESSIONS_HOME` — relocates the whole runtime directory.
    pub sessions_home: Option<PathBuf>,
    /// `CLAUDE_SESSIONS_DB` — moves just the SQLite file.
    pub db: Option<PathBuf>,
    /// `CLAUDE_PROJECTS_DIR` — relocates Claude Code's transcript store.
    pub projects_dir: Option<PathBuf>,
    /// `QA_SCREENSHOT_ROOT` — the QAden plugin's own override, read here so the
    /// run-state reader and QAden itself agree on where a task's QA directory
    /// is.
    pub qa_root: Option<PathBuf>,
}

impl PathEnv {
    pub fn from_env() -> Self {
        PathEnv {
            sessions_home: env_path("CLAUDE_SESSIONS_HOME"),
            db: env_path("CLAUDE_SESSIONS_DB"),
            projects_dir: env_path("CLAUDE_PROJECTS_DIR"),
            qa_root: env_path("QA_SCREENSHOT_ROOT"),
        }
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    path_value(env::var_os(key))
}

/// An empty variable means "unset" — an exported-but-blank value should not
/// silently point state at the filesystem root.
fn path_value(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Resolved locations. Cheap to clone; nothing here touches the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The user's home directory, kept because `~` expansion on save needs it.
    pub home: PathBuf,
    /// `~/.claude-sessions` unless `CLAUDE_SESSIONS_HOME` says otherwise.
    pub runtime_dir: PathBuf,
    pub tasks_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub prompts_dir: PathBuf,
    /// Where a spawned agent writes the plain-English summary the pipeline
    /// prompt asks for. The Node app hardcoded
    /// `/tmp/claude-sessions-task-<id>-summary.md`; keeping it with the rest of
    /// the runtime state means an isolated run cannot read or overwrite the
    /// real one's summaries.
    pub summaries_dir: PathBuf,
    pub done_dir: PathBuf,
    pub blocked_dir: PathBuf,
    /// `notify.json` — how the helper CLIs find the daemon's port.
    pub port_file: PathBuf,
    pub db_path: PathBuf,
    /// `~/.claude`.
    pub claude_dir: PathBuf,
    /// Claude Code's transcript store, `~/.claude/projects` by default.
    pub projects_dir: PathBuf,
    /// `~/.claude/todos`.
    pub todos_dir: PathBuf,
    /// Where the QAden plugin keeps a task's QA run: `~/Desktop/QAden` unless
    /// `QA_SCREENSHOT_ROOT` says otherwise. Read-only to this crate.
    pub qa_root: PathBuf,
    /// The separate auto-dev-daemon's per-task run logs. Read-only, and absent
    /// on any machine that does not run that daemon.
    pub auto_dev_runs_dir: PathBuf,
    /// `~/.claude-sessions.json`. Note it sits beside the runtime directory
    /// rather than inside it, which is where the Node app kept it.
    pub config_path: PathBuf,
    is_isolated: bool,
}

impl Paths {
    /// Reads the environment. Call this once, in `main`.
    pub fn from_env() -> Self {
        Paths::resolve(&home_dir(), &PathEnv::from_env())
    }

    /// The same resolution rules against an explicit home and environment, so
    /// they can be tested without exporting anything.
    pub fn resolve(home: &Path, env: &PathEnv) -> Self {
        let default_runtime = home.join(".claude-sessions");
        let runtime_dir = env
            .sessions_home
            .clone()
            .unwrap_or_else(|| default_runtime.clone());
        let claude_dir = home.join(".claude");
        Paths {
            tasks_dir: runtime_dir.join("tasks"),
            logs_dir: runtime_dir.join("logs"),
            prompts_dir: runtime_dir.join("prompts"),
            summaries_dir: runtime_dir.join("summaries"),
            done_dir: runtime_dir.join("done"),
            blocked_dir: runtime_dir.join("blocked"),
            port_file: runtime_dir.join("notify.json"),
            db_path: env
                .db
                .clone()
                .unwrap_or_else(|| runtime_dir.join("claude-sessions.db")),
            projects_dir: env
                .projects_dir
                .clone()
                .unwrap_or_else(|| claude_dir.join("projects")),
            todos_dir: claude_dir.join("todos"),
            qa_root: env
                .qa_root
                .clone()
                .unwrap_or_else(|| home.join("Desktop").join("QAden")),
            auto_dev_runs_dir: home
                .join(".local")
                .join("share")
                .join("auto-dev-daemon")
                .join("runs"),
            config_path: home.join(".claude-sessions.json"),
            is_isolated: runtime_dir != default_runtime,
            claude_dir,
            runtime_dir,
            home: home.to_path_buf(),
        }
    }

    /// A throwaway tree under `root`, with the config file and the transcript
    /// store inside it too.
    ///
    /// The transcript store matters as much as the runtime directory: the
    /// task-to-transcript search reads every file it finds there, so a test
    /// pointed at the real one would depend on whichever tasks the person
    /// running it happened to work on.
    pub fn for_test(root: &Path) -> Self {
        let mut paths = Paths::resolve(
            root,
            &PathEnv {
                sessions_home: Some(root.join("runtime")),
                db: None,
                projects_dir: Some(root.join("projects")),
                qa_root: Some(root.join("qaden")),
            },
        );
        paths.claude_dir = root.join(".claude");
        paths.todos_dir = paths.claude_dir.join("todos");
        paths
    }

    /// True when state has been redirected away from the user's real directory.
    /// Surfaces in the UI so a run against a temp tree is never mistaken for the
    /// real dashboard.
    pub fn is_isolated(&self) -> bool {
        self.is_isolated
    }

    /// Where a finished task's archived transcripts live.
    pub fn task_dir(&self, task_id: i64) -> PathBuf {
        self.tasks_dir.join(task_id.to_string())
    }

    /// The marker file `claude-sessions done <id>` writes and the daemon watches.
    pub fn done_marker(&self, task_id: i64) -> PathBuf {
        self.done_dir.join(format!("{task_id}.json"))
    }

    /// Where the agent writes a finished task's summary, named into the
    /// prompt so the completion command can read it back.
    pub fn task_summary_file(&self, task_id: i64) -> PathBuf {
        self.summaries_dir
            .join(format!("task-{task_id}-summary.md"))
    }

    /// A task's QAden directory, `task-<id>-qa` under the QA root.
    pub fn qa_task_dir(&self, task_id: i64) -> PathBuf {
        self.qa_root.join(format!("task-{task_id}-qa"))
    }

    /// The marker file `claude-sessions blocked <id>` writes.
    pub fn blocked_marker(&self, task_id: i64) -> PathBuf {
        self.blocked_dir.join(format!("{task_id}.json"))
    }

    /// Claude Code stores a project's transcripts under the cwd with every `/`
    /// replaced by `-`; see [`crate::util::cwd_to_project_dir`].
    ///
    /// On Windows the name is found by reading the listing instead — see
    /// [`project_dir_for`].
    pub fn project_transcripts(&self, cwd: &str) -> PathBuf {
        project_dir_for(&self.projects_dir, cwd, MATCH_BY_LISTING)
    }
}

/// Whether a cwd has to be matched against the directory listing rather than
/// encoded into a name directly.
///
/// On Unix the encoding is known and exercised by every session on every
/// machine this tool has ever run on: `/` becomes `-`, and the name can be
/// built without reading anything. On Windows it is NOT known. The documented
/// rule says "every non-alphanumeric character becomes `-`", which would make
/// `C:\Users\jane` into `C--Users-jane`; Claude Code might equally collapse the
/// run and write `C-Users-jane`, and no build here has been able to check
/// against a real install. Guessing wrong means a Windows machine finds no
/// transcripts at all and the sessions list is silently empty.
///
/// So on Windows the guess is not made: the directory that is already there is
/// matched back to the cwd instead, which is right for either spelling and for
/// any spelling a future release picks.
const MATCH_BY_LISTING: bool = cfg!(windows);

/// The transcript directory for `cwd` under `projects_dir`.
///
/// `by_listing` is [`MATCH_BY_LISTING`], passed rather than read so the Windows
/// behaviour is testable everywhere. With it off this is one `join` and no
/// syscall — the Unix path, unchanged and deliberately cheap, because the
/// scanner asks this per project per tick.
///
/// With it on, the listing decides: both sides are reduced to their letters and
/// digits ([`normalise_name`]) and the directory whose name reduces to the same
/// thing as the cwd wins. `C--Users-jane-app`, `C-Users-jane-app` and
/// `C:\Users\jane\app` all reduce to `cusersjaneapp`, so any of the plausible
/// encodings resolves without this file knowing which one is real. Ties go to
/// the first name in sort order, so the answer does not depend on readdir
/// order.
///
/// The encoded name is still the answer when nothing matches — a project whose
/// directory does not exist yet has to resolve to something, and an empty
/// directory reads as no sessions either way.
pub(crate) fn project_dir_for(projects_dir: &Path, cwd: &str, by_listing: bool) -> PathBuf {
    let encoded = projects_dir.join(crate::util::cwd_to_project_dir(cwd));
    if !by_listing {
        return encoded;
    }
    matching_project_dir(projects_dir, cwd).unwrap_or(encoded)
}

fn matching_project_dir(projects_dir: &Path, cwd: &str) -> Option<PathBuf> {
    let target = crate::util::normalise_name(cwd);
    if target.is_empty() {
        return None;
    }
    std::fs::read_dir(projects_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| crate::util::normalise_name(&name.to_string_lossy()) == target)
        .min()
        .map(|name| projects_dir.join(name))
}

/// The variables that name the user's home directory, most authoritative first.
///
/// `$HOME` is what Node's `os.homedir()` consults first on Unix, and it is what
/// a test harness sets. On Windows `%USERPROFILE%` is the one the system always
/// sets and the one Claude Code itself writes `.claude` under, so it wins
/// there — but `$HOME` is still consulted after it, because a Git-Bash or MSYS
/// shell sets one and a person working in one expects it to mean something.
const HOME_KEYS: &[&str] = if cfg!(windows) {
    &["USERPROFILE", "HOME"]
} else {
    &["HOME"]
};

/// The user's home directory, falling back to the current directory.
///
/// Deliberately not `std::env::home_dir` (deprecated on the 1.85 baseline) and
/// deliberately not a crate — a crate for two `getenv`s is a dependency in the
/// install path for nothing.
fn home_dir() -> PathBuf {
    first_path(HOME_KEYS.iter().map(|key| env_path(key)))
}

/// The first variable that is set to something, or the current directory.
///
/// Split out so the precedence rule can be tested without exporting anything:
/// there is no way to watch `%USERPROFILE%` lose to nothing on a machine that
/// has it set.
fn first_path(values: impl IntoIterator<Item = Option<PathBuf>>) -> PathBuf {
    values
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(home: Option<&str>, db: Option<&str>, projects: Option<&str>) -> PathEnv {
        PathEnv {
            sessions_home: home.map(PathBuf::from),
            db: db.map(PathBuf::from),
            projects_dir: projects.map(PathBuf::from),
            qa_root: None,
        }
    }

    #[test]
    fn defaults_hang_off_the_home_directory() {
        let p = Paths::resolve(Path::new("/home/dev"), &PathEnv::default());
        assert_eq!(p.runtime_dir, Path::new("/home/dev/.claude-sessions"));
        assert_eq!(p.tasks_dir, Path::new("/home/dev/.claude-sessions/tasks"));
        assert_eq!(p.logs_dir, Path::new("/home/dev/.claude-sessions/logs"));
        assert_eq!(
            p.prompts_dir,
            Path::new("/home/dev/.claude-sessions/prompts")
        );
        assert_eq!(p.done_dir, Path::new("/home/dev/.claude-sessions/done"));
        assert_eq!(
            p.blocked_dir,
            Path::new("/home/dev/.claude-sessions/blocked")
        );
        assert_eq!(
            p.port_file,
            Path::new("/home/dev/.claude-sessions/notify.json")
        );
        assert_eq!(
            p.db_path,
            Path::new("/home/dev/.claude-sessions/claude-sessions.db")
        );
        assert_eq!(p.projects_dir, Path::new("/home/dev/.claude/projects"));
        assert_eq!(p.todos_dir, Path::new("/home/dev/.claude/todos"));
        assert_eq!(p.config_path, Path::new("/home/dev/.claude-sessions.json"));
        assert!(!p.is_isolated());
    }

    #[test]
    fn sessions_home_relocates_every_runtime_path() {
        let p = Paths::resolve(
            Path::new("/home/dev"),
            &env_with(Some("/tmp/cs"), None, None),
        );
        // The Node bug this fixes: done/blocked/notify joined onto homedir()
        // directly, so these three stayed in the real directory.
        assert_eq!(p.done_dir, Path::new("/tmp/cs/done"));
        assert_eq!(p.blocked_dir, Path::new("/tmp/cs/blocked"));
        assert_eq!(p.port_file, Path::new("/tmp/cs/notify.json"));
        assert_eq!(p.db_path, Path::new("/tmp/cs/claude-sessions.db"));
        assert!(p.is_isolated());
    }

    #[test]
    fn db_and_projects_overrides_win_over_the_runtime_dir() {
        let p = Paths::resolve(
            Path::new("/home/dev"),
            &env_with(Some("/tmp/cs"), Some("/tmp/other.db"), Some("/tmp/proj")),
        );
        assert_eq!(p.db_path, Path::new("/tmp/other.db"));
        assert_eq!(p.projects_dir, Path::new("/tmp/proj"));
        // …but the rest still follows CLAUDE_SESSIONS_HOME.
        assert_eq!(p.tasks_dir, Path::new("/tmp/cs/tasks"));
    }

    #[test]
    fn a_home_pointed_at_the_default_is_not_isolation() {
        let p = Paths::resolve(
            Path::new("/home/dev"),
            &env_with(Some("/home/dev/.claude-sessions"), None, None),
        );
        assert!(!p.is_isolated());
    }

    #[test]
    fn for_test_keeps_everything_inside_the_temp_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Paths::for_test(tmp.path());
        for path in [
            &p.runtime_dir,
            &p.db_path,
            &p.done_dir,
            &p.projects_dir,
            &p.todos_dir,
            &p.qa_root,
            &p.config_path,
        ] {
            assert!(
                path.starts_with(tmp.path()),
                "{} escaped the temp tree",
                path.display()
            );
        }
        assert!(p.is_isolated());
    }

    #[test]
    fn the_qa_root_defaults_beside_the_desktop_and_follows_its_own_variable() {
        // QAden reads QA_SCREENSHOT_ROOT; so must the reader that follows it,
        // or a relocated QA directory silently reports "never QA'd".
        let plain = Paths::resolve(Path::new("/home/dev"), &PathEnv::default());
        assert_eq!(plain.qa_root, Path::new("/home/dev/Desktop/QAden"));
        assert_eq!(
            plain.qa_task_dir(6440),
            Path::new("/home/dev/Desktop/QAden/task-6440-qa")
        );
        assert_eq!(
            plain.auto_dev_runs_dir,
            Path::new("/home/dev/.local/share/auto-dev-daemon/runs")
        );

        let moved = Paths::resolve(
            Path::new("/home/dev"),
            &PathEnv {
                qa_root: Some(PathBuf::from("/tmp/qa")),
                ..PathEnv::default()
            },
        );
        assert_eq!(moved.qa_task_dir(1), Path::new("/tmp/qa/task-1-qa"));
    }

    #[test]
    fn markers_and_task_dirs_are_named_after_the_task() {
        let p = Paths::resolve(Path::new("/home/dev"), &PathEnv::default());
        assert_eq!(
            p.done_marker(4033),
            Path::new("/home/dev/.claude-sessions/done/4033.json")
        );
        assert_eq!(
            p.blocked_marker(4033),
            Path::new("/home/dev/.claude-sessions/blocked/4033.json")
        );
        assert_eq!(
            p.task_dir(4033),
            Path::new("/home/dev/.claude-sessions/tasks/4033")
        );
        assert_eq!(
            p.task_summary_file(4033),
            Path::new("/home/dev/.claude-sessions/summaries/task-4033-summary.md")
        );
    }

    #[test]
    fn transcripts_live_under_the_encoded_cwd() {
        let p = Paths::resolve(Path::new("/home/dev"), &PathEnv::default());
        assert_eq!(
            p.project_transcripts("/Users/k/dev/app"),
            Path::new("/home/dev/.claude/projects/-Users-k-dev-app")
        );
    }

    /// A projects directory holding one arbitrarily-named transcript folder.
    fn projects_holding(name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(name)).expect("mkdir");
        dir
    }

    #[test]
    fn the_unix_path_is_built_not_looked_up() {
        // No syscall, and the answer does not depend on what is on disk: a
        // directory spelled some other way is NOT what `/Users/k/dev/app`
        // resolves to, and a project with no directory yet still resolves.
        let dir = projects_holding("-Users-k-dev-app-something-else");
        assert_eq!(
            project_dir_for(dir.path(), "/Users/k/dev/app", false),
            dir.path().join("-Users-k-dev-app")
        );
    }

    #[test]
    fn a_windows_cwd_resolves_to_whichever_spelling_is_on_disk() {
        // The encoding Claude Code uses for a drive letter is NOT confirmed:
        // `C:\Users\jane\app` could be written either of these ways. Both
        // resolve, because the listing is matched back to the cwd instead of a
        // name being guessed — which is the whole point, since guessing wrong
        // means an empty sessions list on a machine that is running all day.
        for spelling in ["C--Users-jane-app", "C-Users-jane-app"] {
            let dir = projects_holding(spelling);
            assert_eq!(
                project_dir_for(dir.path(), "C:\\Users\\jane\\app", true),
                dir.path().join(spelling),
                "{spelling} did not resolve"
            );
        }
    }

    #[test]
    fn matching_by_listing_ignores_punctuation_and_case_but_not_the_path() {
        let dir = projects_holding("c--users-jane-app");
        // Same directory, whatever separators the cwd was written with.
        for cwd in ["C:\\Users\\Jane\\app", "C:/Users/Jane/app"] {
            assert_eq!(
                project_dir_for(dir.path(), cwd, true),
                dir.path().join("c--users-jane-app")
            );
        }
        // A different project is not matched to it: it falls back to the
        // encoded name, whichever way this build spells one.
        let other = "C:\\Users\\jane\\other";
        assert_eq!(
            project_dir_for(dir.path(), other, true),
            dir.path().join(crate::util::cwd_to_project_dir(other))
        );
    }

    #[test]
    fn an_unmatched_cwd_falls_back_to_the_encoded_name() {
        // Nothing on disk yet — a project nobody has run Claude Code in. The
        // name has to be relative, or `join` would drop the base and point the
        // scan at the repository itself.
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = "C:\\Users\\jane\\app";
        let resolved = project_dir_for(dir.path(), cwd, true);
        assert_eq!(
            resolved,
            dir.path().join(crate::util::cwd_to_project_dir(cwd))
        );
        assert!(resolved.starts_with(dir.path()), "{}", resolved.display());
        assert_eq!(project_dir_for(dir.path(), "", true), dir.path().to_owned());
    }

    #[test]
    fn blank_environment_values_are_treated_as_unset() {
        // An exported-but-empty CLAUDE_SESSIONS_HOME must not resolve to "/".
        assert_eq!(path_value(Some("".into())), None);
        assert_eq!(path_value(None), None);
        assert_eq!(
            path_value(Some("/tmp/cs".into())),
            Some(PathBuf::from("/tmp/cs"))
        );
    }

    #[test]
    fn home_takes_the_first_variable_that_is_set() {
        let set = |value: &str| Some(PathBuf::from(value));
        assert_eq!(first_path([None, set("/second")]), Path::new("/second"));
        assert_eq!(
            first_path([set("/first"), set("/second")]),
            Path::new("/first")
        );
        // A machine with none of them set still resolves to somewhere, so that
        // nothing downstream has to handle the absence of a home directory.
        assert_eq!(first_path([None, None]), Path::new("."));
    }

    #[test]
    fn windows_prefers_the_profile_directory_and_still_honours_home() {
        // %USERPROFILE% is always set on Windows and is where Claude Code keeps
        // `.claude`; $HOME is only set by a shell that emulates one.
        let expected: &[&str] = if cfg!(windows) {
            &["USERPROFILE", "HOME"]
        } else {
            &["HOME"]
        };
        assert_eq!(HOME_KEYS, expected);
    }
}
