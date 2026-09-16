//! The repositories inside a configured group directory.

use std::fs;
use std::path::Path;

/// Directories that are never a project, whatever they are called. Dotfiles are
/// skipped separately, so `.git` is here only for the case of a group path that
/// points straight at a checkout.
const IGNORED: [&str; 9] = [
    "node_modules",
    "__pycache__",
    ".git",
    ".cache",
    ".venv",
    ".env",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
];

/// One level of `group_path`: the subdirectories that could be projects, sorted.
///
/// Deliberately not recursive. A group is a folder of checkouts, and walking
/// into them would mean walking `node_modules`. An unreadable group is an empty
/// list, not an error — a group configured for a disk that is not mounted
/// should leave the rest of the dashboard alone.
pub fn discover_projects(group_path: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(group_path) else {
        return Vec::new();
    };
    let mut dirs: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || IGNORED.contains(&name.as_str()) {
                return None;
            }
            // Follows symlinks, so a symlinked worktree is a project.
            fs::metadata(entry.path())
                .ok()
                .filter(|meta| meta.is_dir())
                .map(|_| name)
        })
        .collect();
    dirs.sort();
    dirs
}
