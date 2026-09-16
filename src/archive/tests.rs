//! Ported from the Node app's `test/task-transcript-crosslink.test.js`,
//! `test/auto-archive.test.js` and the archive half of
//! `test/task-session-linking.test.js`, plus the cases the SQLite index made
//! possible.
//!
//! Every test builds its own runtime tree AND its own transcript store through
//! [`Paths::for_test`]. The store matters as much as the runtime directory:
//! [`Archive::find_task_session_anywhere`] reads every transcript it finds
//! there, so a test pointed at the real one would depend on whichever tasks the
//! person running it happened to work on. That is the structural version of the
//! Node suite's `helpers/isolate.js`, which had to be imported before anything
//! else to patch the environment in time.

mod crosslink;
mod index;
mod resume;
mod search;
mod store;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::*;
use crate::types::SessionFile;
use crate::util::cwd_to_project_dir;

const HOUR: Duration = Duration::from_secs(3600);

/// One archive over a throwaway tree, with the transcript-head cache a daemon
/// shares between the scanner and this module.
struct TestArchive {
    _dir: TempDir,
    paths: Paths,
    db: Db,
    refs: TaskRefCache,
    seq: u32,
}

fn open() -> TestArchive {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    let db = Db::open(&paths);
    assert!(db.available(), "temp database: {:?}", db.first_error());
    TestArchive {
        _dir: dir,
        paths,
        db,
        refs: TaskRefCache::new(),
        seq: 0,
    }
}

// The calls under test, with the borrow split written once: `Archive` holds the
// paths and the database while the head cache is handed in separately, which is
// exactly how the daemon will hold them (its Scanner owns the cache).
impl TestArchive {
    fn archive_task(&mut self, task_id: i64, request: &ArchiveRequest) -> Option<TaskArchive> {
        Archive::new(&self.paths, &self.db).archive_task_conversation(
            task_id,
            request,
            &mut self.refs,
        )
    }

    fn ensure_live(&mut self, task_id: i64) -> Option<TaskArchive> {
        Archive::new(&self.paths, &self.db).ensure_live_session(task_id, &mut self.refs)
    }

    fn find_in(&mut self, cwd: &str, task_id: i64) -> Option<TaskSessionFile> {
        Archive::new(&self.paths, &self.db).find_task_session_file(cwd, task_id, &mut self.refs)
    }

    fn find_near(&mut self, cwd: &str, task_id: i64) -> Option<TaskSessionFile> {
        Archive::new(&self.paths, &self.db).find_task_session_near(cwd, task_id, &mut self.refs)
    }

    fn find_anywhere(&mut self, task_id: i64) -> Option<TaskSessionFile> {
        Archive::new(&self.paths, &self.db).find_task_session_anywhere(task_id, &mut self.refs)
    }

    fn belongs_to_other(&mut self, file: &Path, task_id: i64) -> bool {
        Archive::new(&self.paths, &self.db).belongs_to_other_task(file, task_id, &mut self.refs)
    }

    fn meta(&self, task_id: i64) -> Option<TaskArchive> {
        Archive::new(&self.paths, &self.db).task_meta(task_id)
    }

    fn archive_path(&self, task_id: i64) -> Option<PathBuf> {
        Archive::new(&self.paths, &self.db).archive_path(task_id)
    }
}

// Fixtures.
impl TestArchive {
    /// A transcript in the transcript store, under the project directory for
    /// `cwd`, whose opening prompt names `task_id` — the shape a spawned agent
    /// writes. `None` is a session started by hand, which carries no task
    /// reference at all.
    fn transcript(&mut self, task_id: Option<i64>, cwd: &str) -> TaskSessionFile {
        self.write_transcript(task_id, cwd, Duration::ZERO)
    }

    /// The same, backdated, so "newest wins" can be asserted without depending
    /// on how fast the test ran.
    fn aged(&mut self, task_id: Option<i64>, cwd: &str, age: Duration) -> TaskSessionFile {
        self.write_transcript(task_id, cwd, age)
    }

    fn write_transcript(
        &mut self,
        task_id: Option<i64>,
        cwd: &str,
        age: Duration,
    ) -> TaskSessionFile {
        self.seq += 1;
        let dir = self.paths.projects_dir.join(cwd_to_project_dir(cwd));
        fs::create_dir_all(&dir).unwrap();
        let session_id = match task_id {
            Some(id) => format!("sess-{id}-{}", self.seq),
            None => format!("sess-plain-{}", self.seq),
        };
        let name = format!("{session_id}.jsonl");
        let path = dir.join(&name);
        let prompt = match task_id {
            // The reference as it really appears: inside the spawn prompt's URL.
            Some(id) => format!(
                "Work this Odoo task end-to-end. https://odoo/web#id={id}&model=project.task&view_type=form"
            ),
            None => "No task here, this session was started by hand.".to_string(),
        };
        let line = serde_json::json!({
            "type": "user",
            "userType": "external",
            "sessionId": session_id,
            "cwd": cwd,
            "message": { "role": "user", "content": prompt },
        });
        fs::write(&path, format!("{line}\n")).unwrap();
        let mtime = SystemTime::now() - age;
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        TaskSessionFile {
            file: SessionFile {
                name,
                path,
                mtime,
                size: 0,
                birthtime: None,
            },
            cwd: cwd.to_string(),
        }
    }

    /// A transcript that is NOT in the transcript store, so no search can find
    /// it and only a caller handing it over can put it forward — which is how
    /// the cross-linked archive happened.
    fn stray_transcript(&mut self, task_id: Option<i64>) -> PathBuf {
        let written = self.transcript(task_id, "/repo/stray");
        let moved = self.paths.runtime_dir.join(&written.file.name);
        fs::create_dir_all(&self.paths.runtime_dir).unwrap();
        fs::rename(written.path(), &moved).unwrap();
        moved
    }

    /// An archive record as some earlier run left it — including the wrong ones
    /// this module has to survive.
    fn record(&self, task_id: i64, cwd: &str, session_id: &str, session_file: &Path) {
        self.db.put_task_archive(&TaskArchive {
            task_id,
            cwd: cwd.to_string(),
            session_id: session_id.to_string(),
            session_file: session_file.to_string_lossy().into_owned(),
            archived_at: crate::util::iso_now(),
        });
    }

    /// What a caller hands in when it believes it knows the session.
    fn linked(&self, cwd: &str, file: &Path) -> ArchiveRequest {
        ArchiveRequest {
            cwd: cwd.to_string(),
            session_file: Some(file.to_path_buf()),
            ..ArchiveRequest::default()
        }
    }
}
