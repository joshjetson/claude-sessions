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
}

impl PathEnv {
    pub fn from_env() -> Self {
        PathEnv {
            sessions_home: env_path("CLAUDE_SESSIONS_HOME"),
            db: env_path("CLAUDE_SESSIONS_DB"),
            projects_dir: env_path("CLAUDE_PROJECTS_DIR"),
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

    /// The marker file `claude-sessions blocked <id>` writes.
    pub fn blocked_marker(&self, task_id: i64) -> PathBuf {
        self.blocked_dir.join(format!("{task_id}.json"))
    }

    /// Claude Code stores a project's transcripts under the cwd with every `/`
    /// replaced by `-`; see [`crate::util::cwd_to_project_dir`].
    pub fn project_transcripts(&self, cwd: &str) -> PathBuf {
        self.projects_dir.join(crate::util::cwd_to_project_dir(cwd))
    }
}

/// `$HOME`, falling back to the current directory.
///
/// Deliberately not `std::env::home_dir` (deprecated on the 1.85 baseline) and
/// deliberately not a crate: the tool is macOS/Linux only, and `$HOME` is what
/// Node's `os.homedir()` consults first anyway.
fn home_dir() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(home: Option<&str>, db: Option<&str>, projects: Option<&str>) -> PathEnv {
        PathEnv {
            sessions_home: home.map(PathBuf::from),
            db: db.map(PathBuf::from),
            projects_dir: projects.map(PathBuf::from),
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
    }

    #[test]
    fn transcripts_live_under_the_encoded_cwd() {
        let p = Paths::resolve(Path::new("/home/dev"), &PathEnv::default());
        assert_eq!(
            p.project_transcripts("/Users/k/dev/app"),
            Path::new("/home/dev/.claude/projects/-Users-k-dev-app")
        );
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
}
