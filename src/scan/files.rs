//! A project's transcripts on disk, and whether one of them is being compacted.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::types::SessionFile;

/// The transcripts in one project directory, newest write first.
///
/// `agent-*.jsonl` are subagent side-files, not sessions anybody drives, so
/// they are filtered out here — which also keeps the `agent-acompact-*` files
/// [`is_compacting`] looks for from being mistaken for transcripts.
pub fn session_files_in(dir: &Path) -> Vec<SessionFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new(); // no directory yet is not an error, just no sessions
    };
    let mut files: Vec<SessionFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".jsonl") || name.starts_with("agent-") {
                return None;
            }
            let path = entry.path();
            // `fs::metadata`, not `DirEntry::metadata`: the latter does not
            // follow symlinks, and a restored archive can leave one behind.
            let meta = fs::metadata(&path).ok()?;
            Some(SessionFile {
                name,
                path,
                mtime: meta.modified().ok()?,
                size: meta.len(),
                birthtime: meta.created().ok(),
            })
        })
        .collect();
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    files
}

#[derive(Debug)]
struct Listing {
    /// `None` when the directory could not be stat'd, which is how a directory
    /// that does not exist yet is remembered.
    dir_mtime: Option<SystemTime>,
    files: Vec<SessionFile>,
}

/// Directory listings kept across ticks, keyed on the directory's own mtime.
///
/// Big-O mandate #2. Node ran a readdir, a stat per file and a sort for every
/// project on every tick — a project with four hundred transcripts and two live
/// sessions paid four hundred stats a second. A directory's mtime changes when
/// a transcript is created or removed, which is exactly when the listing needs
/// rebuilding, so an unchanged directory now costs one stat.
///
/// What it deliberately does NOT track is a transcript being appended to: that
/// leaves the directory's mtime alone, so the `mtime` and `size` on a cached
/// entry go stale within a tick or two. They are used here for ORDERING and
/// IDENTITY only; the scanner re-stats the handful of files it actually paired,
/// because the session status machine reads that mtime and needs it fresh.
#[derive(Debug, Default)]
pub struct SessionFilesCache {
    dirs: HashMap<PathBuf, Listing>,
    builds: u64,
}

impl SessionFilesCache {
    pub fn new() -> Self {
        SessionFilesCache::default()
    }

    /// The listing for `dir`, reading the directory only when it has changed.
    pub fn list(&mut self, dir: &Path) -> &[SessionFile] {
        let dir_mtime = fs::metadata(dir).ok().and_then(|m| m.modified().ok());
        let fresh = match self.dirs.get(dir) {
            Some(cached) => match dir_mtime {
                Some(_) => cached.dir_mtime == dir_mtime,
                // Still missing: no point reading a directory that is not there.
                None => cached.dir_mtime.is_none(),
            },
            None => false,
        };
        if !fresh {
            self.builds += 1;
            self.dirs.insert(
                dir.to_path_buf(),
                Listing {
                    dir_mtime,
                    files: session_files_in(dir),
                },
            );
        }
        self.dirs
            .get(dir)
            .map(|listing| listing.files.as_slice())
            .unwrap_or_default()
    }

    /// Forget every directory outside `keep` — called with the directories a
    /// tick actually looked at, so the cache stays the size of the live board
    /// rather than of everything ever opened.
    pub fn prune(&mut self, keep: &HashSet<PathBuf>) {
        self.dirs.retain(|dir, _| keep.contains(dir));
    }

    /// How many times a directory has actually been read. The counter the
    /// mandate above is tested against.
    pub fn builds(&self) -> u64 {
        self.builds
    }
}

/// A transcript is compacted by a sidecar agent that writes
/// `agent-acompact-*.jsonl` next to it, so a recent one means this session is
/// busy summarising itself rather than idle.
const COMPACT_WINDOW: Duration = Duration::from_secs(30);

/// Whether the session at `session_file` is being compacted right now.
///
/// The caller owns the throttling — the daemon holds the answer for a couple of
/// seconds, since this reads a directory.
pub fn is_compacting(session_file: &Path, now: SystemTime) -> bool {
    let Some(dir) = session_file.parent() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("agent-acompact-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Some(mtime) = fs::metadata(entry.path())
            .ok()
            .and_then(|m| m.modified().ok())
        else {
            continue;
        };
        // A file stamped in the future counts as recent, as it did in Node,
        // rather than reading as thirty years old.
        match now.duration_since(mtime) {
            Ok(age) if age < COMPACT_WINDOW => return true,
            Err(_) => return true,
            _ => {}
        }
    }
    false
}
