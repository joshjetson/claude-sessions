//! Ported from the Node app's `test/alerts.test.js`,
//! `test/task-session-linking.test.js`, `test/awaiting.test.js`,
//! `test/auto-archive.test.js` and the engine half of `test/daemon.test.js`.
//!
//! The wire half — `protocol`, `server`, `client`, `cli` — is the rest of
//! `test/daemon.test.js`: every server there binds an ephemeral port on
//! loopback and no test starts a real daemon.
//!
//! Every test gets its own runtime tree AND its own transcript store through
//! [`Paths::for_test`] — the structural version of the Node suite's
//! `helpers/isolate.js`, which had to be imported before anything else to patch
//! the environment in time. Nothing here reaches Odoo, opens a socket or spawns
//! a process: the operating system is a [`FakeProcesses`] table, the task
//! backend is a recorder, and the spawn policy refuses.

mod alerts;
mod archive;
mod awaiting;
mod backend;
mod board;
mod cli;
mod client;
mod completion;
mod deploy;
mod fakes;
mod linking;
mod markers;
mod ordering;
mod protocol;
mod refresh;
mod remote;
mod roles;
mod routes;
mod server;
mod transcripts;
mod watchers;
mod wire;

pub(crate) use fakes::{BackendCall, FakeProcesses, RecordingBackend};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::scan::{Discovery, Scanner};

use super::completion::DailyLogRecord;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use super::engine::{Engine, EngineInner, EngineOptions};
use super::state::SessionIndex;
use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::Paths;
use crate::term::SpawnPolicy;
use crate::transcript::TaskRefCache;
use crate::types::{LastEntry, Session, SessionStatus, Task};
use crate::util::{cwd_to_project_dir, project_name};

pub(crate) const MINUTE: Duration = Duration::from_secs(60);

// --- the harness ------------------------------------------------------------

/// One engine over a throwaway tree, never started, so a test drives the tick
/// and the watchers itself.
pub(crate) struct TestEngine {
    _dir: TempDir,
    pub(crate) paths: Paths,
    pub(crate) engine: Engine<FakeProcesses>,
    pub(crate) procs: FakeProcesses,
    pub(crate) backend: RecordingBackend,
    pub(crate) daily_log: Arc<Mutex<Vec<DailyLogRecord>>>,
    /// The head cache a running daemon shares between the scanner and the
    /// archive; handed to the watchers that read transcript heads.
    pub(crate) refs: TaskRefCache,
    seq: u32,
}

/// The engine, with whatever the test wants injected.
#[derive(Default)]
pub(crate) struct Setup {
    pub(crate) config: Option<serde_json::Value>,
    pub(crate) assigned: Option<super::engine::AssignedFetch>,
    pub(crate) qa_stage: Option<super::engine::QaStageFetch>,
    pub(crate) usage: Option<super::engine::UsageHook>,
    pub(crate) board: Option<super::engine::BoardFetch>,
    pub(crate) deploy: Option<super::engine::DeployFetch>,
    /// Refused everywhere but the one test that drives a real child process.
    pub(crate) spawn: Option<SpawnPolicy>,
    /// How sessions are discovered. `None` is the process table, which is what
    /// every test but the transcript-only ones wants — they run the layer a
    /// machine with no readable process table uses, on this one.
    pub(crate) discovery: Option<Discovery>,
}

pub(crate) fn engine() -> TestEngine {
    engine_with(Setup::default())
}

pub(crate) fn engine_with(setup: Setup) -> TestEngine {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).unwrap();
    fs::create_dir_all(&paths.projects_dir).unwrap();
    if let Some(config) = &setup.config {
        fs::write(&paths.config_path, serde_json::to_string(config).unwrap()).unwrap();
    }
    let config = ConfigHandle::load_from(&paths.config_path, &paths.home, EnvOverrides::default());

    let procs = FakeProcesses::new();
    let backend = RecordingBackend::default();
    let daily_log: Arc<Mutex<Vec<DailyLogRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&daily_log);

    let engine = Engine::new(EngineOptions {
        backend: Arc::new(backend.clone()),
        fetch_assigned: setup.assigned,
        fetch_qa_stage: setup.qa_stage,
        fetch_board: setup.board,
        fetch_deploy: setup.deploy,
        daily_log: Some(Box::new(move |record| {
            log.lock().unwrap().push(record.clone())
        })),
        usage: setup.usage,
        // Refused everywhere but the one deploy test that deliberately runs a
        // real short-lived `sh`.
        spawn: setup.spawn.unwrap_or(SpawnPolicy::Refuse),
        ..EngineOptions::with_scanner(
            paths.clone(),
            config,
            Scanner::new(
                procs.clone(),
                paths.clone(),
                setup.discovery.unwrap_or(Discovery::Processes),
            ),
        )
    });

    TestEngine {
        _dir: dir,
        paths,
        engine,
        procs,
        backend,
        daily_log,
        refs: TaskRefCache::new(),
        seq: 0,
    }
}

impl TestEngine {
    pub(crate) fn inner(&self) -> &Arc<EngineInner<FakeProcesses>> {
        self.engine.inner()
    }

    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, super::state::EngineState> {
        self.inner().state()
    }

    /// Run the vanish sweep over the sessions a tick would have seen. The
    /// `Arc` clone is what lets the shared head cache be passed in while the
    /// engine itself is borrowed — the same borrow split a real tick makes.
    pub(crate) fn auto_archive(&mut self, sessions: Vec<Session>) {
        let live = index(sessions);
        let inner = Arc::clone(self.engine.inner());
        inner.auto_archive_vanished_sessions(&live, &mut self.refs);
    }

    /// A transcript in the transcript store under `cwd`, whose opening prompt
    /// names `task_id` — the shape a spawned agent writes.
    pub(crate) fn transcript(&mut self, task_id: i64, cwd: &str) -> Session {
        self.write_transcript(Some(task_id), cwd, Duration::ZERO)
    }

    pub(crate) fn aged_transcript(&mut self, task_id: i64, cwd: &str, age: Duration) -> Session {
        self.write_transcript(Some(task_id), cwd, age)
    }

    fn write_transcript(&mut self, task_id: Option<i64>, cwd: &str, age: Duration) -> Session {
        self.seq += 1;
        let dir = self.paths.projects_dir.join(cwd_to_project_dir(cwd));
        fs::create_dir_all(&dir).unwrap();
        let session_id = match task_id {
            Some(id) => format!("sess-{id}-{}", self.seq),
            None => format!("sess-plain-{}", self.seq),
        };
        let path = dir.join(format!("{session_id}.jsonl"));
        let prompt = match task_id {
            Some(id) => format!(
                "Work this Odoo task end-to-end. https://odoo/web#id={id}&model=project.task&view_type=form"
            ),
            None => "Started by hand, no task here.".to_string(),
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

        Session {
            session_file: Some(path),
            session_mtime: mtime,
            task_id,
            ..a_session(&session_id, cwd)
        }
    }
}

// --- session fixtures -------------------------------------------------------

/// A live session as an enriched tick reports one.
pub(crate) fn a_session(session_id: &str, cwd: &str) -> Session {
    Session {
        session_id: session_id.to_string(),
        pids: vec![4242],
        cwd: cwd.to_string(),
        tty: Some("ttys001".to_string()),
        lstart: None,
        session_file: Some(PathBuf::from(format!("/transcripts/{session_id}.jsonl"))),
        session_mtime: SystemTime::now(),
        session_size: Some(10),
        status: SessionStatus::Working,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
        run_id: None,
    }
}

/// A `claude` process the scanner cannot pair with a transcript yet.
pub(crate) fn a_placeholder(pid: u32, cwd: &str) -> Session {
    Session {
        session_file: None,
        status: SessionStatus::Starting,
        starting: true,
        ..a_session(&format!("starting-{pid}"), cwd)
    }
}

/// A session whose mtime could not be read at all.
pub(crate) fn no_mtime(session: Session) -> Session {
    Session {
        session_mtime: UNIX_EPOCH,
        ..session
    }
}

pub(crate) fn index(sessions: Vec<Session>) -> SessionIndex {
    SessionIndex::build(sessions, |session| project_name(&session.cwd))
}

/// An assistant entry whose trailing tool call is `tool`.
pub(crate) fn tool_entry(tool: &str) -> LastEntry {
    LastEntry {
        kind: crate::types::EntryKind::Assistant,
        has_message: true,
        tool_uses: vec![tool.to_string()],
        ..LastEntry::default()
    }
}

/// An Odoo task row as the board and the assignment watcher see one.
pub(crate) fn a_task(id: i64, stage: &str) -> Task {
    Task {
        id,
        name: format!("task {id}"),
        project_name: "Project A".to_string(),
        stage_name: stage.to_string(),
        ..Task::default()
    }
}

/// Where a session's transcript went, for asserting against the archive.
pub(crate) fn archived_session_id(engine: &TestEngine, task_id: i64) -> Option<String> {
    engine
        .inner()
        .archive()
        .task_meta(task_id)
        .map(|meta| meta.session_id)
}

pub(crate) fn exists(path: &Path) -> bool {
    path.exists()
}
