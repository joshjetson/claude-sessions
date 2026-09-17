//! The lookups the report is made of: PATH, shebangs, ages, environment.
//!
//! Separated from the report itself so that what is asked and how it is asked
//! can be read apart — and so the two tests that matter here ([`shebang`] in
//! particular, which is what decides whether `ps` shows a session or its
//! interpreter) have somewhere obvious to point.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The first executable of this name on `PATH`.
///
/// Read here rather than shelled out to `which`: it is four lines, it cannot
/// fail for lack of a helper binary, and it is the same PATH this process
/// would launch with — which is the question being asked.
pub(super) fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// The `#!` line, for a file that has one.
pub(crate) fn shebang(path: &Path) -> Option<String> {
    let mut head = [0u8; 256];
    let read = fs::File::open(path).ok()?.read(&mut head).ok()?;
    let rest = head[..read].strip_prefix(b"#!")?;
    let end = rest.iter().position(|byte| *byte == b'\n')?;
    Some(String::from_utf8_lossy(&rest[..end]).trim().to_string())
}

/// How long ago, in the same words the session rows use.
pub(super) fn ago(at: SystemTime) -> String {
    let then = chrono::DateTime::<chrono::Utc>::from(at);
    crate::util::time_ago(then, chrono::Utc::now())
}

pub(super) fn pid_list(pids: &[u32]) -> String {
    match pids.is_empty() {
        true => "none".to_string(),
        false => pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The first of these variables that is set, named, or `unset`. Paths only —
/// nothing here reads a variable that could hold a secret.
pub(super) fn env_or(keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            return format!("{key}={}", value.to_string_lossy());
        }
    }
    "unset".to_string()
}
