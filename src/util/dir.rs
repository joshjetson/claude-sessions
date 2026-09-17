//! Taking a directory path apart, and deciding when two of them mean the same
//! directory.
//!
//! Everything here is string work on paths the tool was handed — by `lsof`, by
//! a transcript, by the config file — and every one of them has to read the
//! same way on both families of separator. The rules live together because they
//! are one rule: a path is a list of segments, and the separator is a platform
//! fact rather than a character somebody typed.
//!
//! What this module is NOT is a path library. Nothing here touches the disk,
//! resolves a symlink or canonicalises anything: these helpers answer string
//! comparisons that run once per session per tick, and a `canonicalize` at that
//! rate would be a syscall storm to decide whether two strings match.

/// What separates one path segment from the next on this platform.
///
/// A backslash is a legal character in a Unix filename, so it is a separator
/// only where it is actually one — splitting on it everywhere would rename
/// somebody's directory out from under them. Named once because everything
/// that takes a path apart consults it: the transcript directory encoding, the
/// project label, the tool-call renderer, the journal's tilde expansion, the
/// editor's basename and the group matching below.
pub const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

/// The characters the transcript directory name encodes away. Windows adds the
/// drive colon, which is illegal in a file name.
const ENCODED: &[char] = if cfg!(windows) {
    &['/', '\\', ':']
} else {
    &['/']
};

/// Whether two path spellings that differ only in case name the same directory.
///
/// They do on Windows, where the filesystem folds case, and they do not on
/// Unix. It matters here because the two sides of a comparison come from
/// different places — a group path a person typed into the config against a
/// working directory Claude Code wrote into a transcript — and `C:\Src` against
/// `c:\src` is a spelling difference on Windows, not a different folder.
const FOLD_CASE: bool = cfg!(windows);

/// Claude Code stores a project's transcripts in a directory named after the
/// cwd with every separator replaced by `-`. Everything that finds a transcript
/// depends on reproducing that encoding exactly.
///
/// On Windows the separator is `\` and a path also carries a drive letter, so
/// both are encoded: `C:\Users\dev\repo` becomes `C--Users-dev-repo`. That
/// the result is RELATIVE matters as much as its spelling — `Path::join`
/// discards the base when handed an absolute component, so a name that kept its
/// drive would point [`crate::paths::Paths::project_transcripts`] at the
/// repository itself instead of at a directory under `~/.claude/projects`, and
/// every stray `.jsonl` in somebody's checkout would read as a session.
///
/// The Windows spelling is NOT confirmed against a real Claude Code install,
/// which is why nothing on that platform depends on it:
/// [`crate::paths::Paths::project_transcripts`] finds the directory by reading
/// the listing and matching it back to the cwd, and reaches for this encoding
/// only as the name a directory that does not exist yet would have.
pub fn cwd_to_project_dir(cwd: &str) -> String {
    cwd.replace(ENCODED, "-")
}

/// A short name for a working directory: its last two meaningful segments, with
/// the scaffolding ones dropped so `/Users/someone/dev/repo` reads `someone/repo`.
pub fn project_name(cwd: &str) -> String {
    if cwd.is_empty() {
        return "unknown".to_string();
    }
    let parts: Vec<&str> = cwd.split(SEPARATORS).filter(|p| !p.is_empty()).collect();
    let meaningful: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| *p != "Users" && *p != "dev" && !is_drive(p))
        .collect();
    if meaningful.len() >= 2 {
        return meaningful[meaningful.len() - 2..].join("/");
    }
    match parts.last() {
        Some(base) => base.to_string(),
        None => cwd.to_string(),
    }
}

/// `C:` and friends — the first segment of an absolute Windows path, and no
/// more a name for a project than `Users` is. Only ever true on Windows, where
/// a directory cannot be called `C:` in the first place.
fn is_drive(part: &str) -> bool {
    cfg!(windows) && {
        let mut chars = part.chars();
        matches!(
            (chars.next(), chars.next(), chars.next()),
            (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic()
        )
    }
}

/// Trailing separators off, so `/a/b/` and `/a/b` compare as the one directory
/// they are.
///
/// Four places compared working directories by hand with
/// `trim_end_matches('/')` — the pending-launch matcher, the board's
/// notification router, the sessions tree and the group discovery in both the
/// daemon and the local feed — and every one of them was blind to `\`. This is
/// that line, once.
///
/// A path that is nothing but separators trims to the empty string, which every
/// caller already treats as "no directory given".
pub fn trim_trailing_separators(path: &str) -> &str {
    path.trim_end_matches(SEPARATORS)
}

/// Whether two paths name the same directory: trailing separators ignored, and
/// case ignored where the filesystem ignores it.
pub fn same_dir(a: &str, b: &str) -> bool {
    path_eq(trim_trailing_separators(a), trim_trailing_separators(b))
}

/// Whether `path` is `dir` itself or sits somewhere beneath it.
pub fn is_within_dir(path: &str, dir: &str) -> bool {
    let path = trim_trailing_separators(path);
    let dir = trim_trailing_separators(dir);
    path_eq(path, dir) || under(path, dir).is_some()
}

/// The immediate subdirectory of `dir` that `path` sits in — `repo` for
/// `/src/repo/app` under `/src` — or `None` when `path` is not beneath `dir` at
/// all.
///
/// The sessions tree groups by exactly this: which checkout inside a configured
/// group folder a session is running in. It used to be a `format!("{dir}/")`
/// prefix test, which on Windows matched nothing, so every Windows session fell
/// through to the ungrouped list at the bottom.
pub fn child_dir_of<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let rest = under(
        trim_trailing_separators(path),
        trim_trailing_separators(dir),
    )?;
    let name = rest.split(SEPARATORS).next().unwrap_or_default();
    (!name.is_empty()).then_some(name)
}

/// The part of `path` below `dir`, with the separator that joins them removed.
/// `None` when `path` does not start at `dir`, and never `Some("")` — a path
/// equal to `dir` is not below it.
fn under<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    if dir.is_empty() {
        return None;
    }
    let head = path.get(..dir.len())?;
    if !path_eq(head, dir) {
        return None;
    }
    let rest = path[dir.len()..].strip_prefix(SEPARATORS)?;
    (!rest.is_empty()).then_some(rest)
}

/// The last segment of a path: `repo` for `/src/repo` and for `C:\src\repo`.
///
/// `Path::file_name` would answer this on the platform the path came from, and
/// answer the whole string on the other one — a Windows cwd arrives here as a
/// string from a transcript written on Windows, so the separator set decides,
/// not the compiler's idea of what a path looks like. A path that ends in
/// separators is read as the directory it names, and a path with no separator
/// in it is its own leaf.
pub fn path_leaf(path: &str) -> &str {
    let trimmed = trim_trailing_separators(path);
    match trimmed.rfind(SEPARATORS) {
        Some(at) => &trimmed[at + 1..],
        None => trimmed,
    }
}

/// `base` and `child` joined with the separator `base` already uses.
///
/// The tree's inactive rows and the launch dialog's folder list both build a
/// path out of a configured group and a discovered directory name, and both
/// used `format!("{base}/{child}")` — which hands a Windows launch a cwd
/// spelled `C:\src/repo`. Tolerated by the API, but it is then compared against
/// working directories spelled the other way, and it is what the person sees.
///
/// The separator is copied from `base` rather than taken from the platform, so
/// a path stays in one spelling end to end: a group configured as `C:\src`
/// grows `\repo`, and one configured as `/srv/src` — or as `~/dev`, which
/// expands with whatever the home directory uses — grows `/repo`.
pub fn join_dir(base: &str, child: &str) -> String {
    if base.is_empty() {
        return child.to_string();
    }
    let trimmed = trim_trailing_separators(base);
    // `/` and `C:\` are roots: the separator is part of the path rather than
    // padding to trim, and `C:` on its own is drive-RELATIVE on Windows, which
    // is not what a group called `C:\` meant.
    if trimmed.is_empty() || trimmed.ends_with(':') {
        let root = (trimmed.len() + 1).min(base.len());
        return format!("{}{child}", &base[..root]);
    }
    format!("{trimmed}{}{child}", separator_in(trimmed))
}

/// The separator a path already uses — the last one in it — falling back to the
/// platform's own where there is none to copy.
fn separator_in(path: &str) -> char {
    path.rfind(SEPARATORS)
        .and_then(|at| path[at..].chars().next())
        .unwrap_or(if cfg!(windows) { '\\' } else { '/' })
}

/// Segment-for-segment equality under this platform's case rules. Not a
/// substitute for [`same_dir`]: it compares what it is handed, trailing
/// separators included.
fn path_eq(a: &str, b: &str) -> bool {
    if FOLD_CASE {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}
