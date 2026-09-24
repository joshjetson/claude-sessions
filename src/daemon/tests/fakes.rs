//! The two things the engine talks to that a test must not: the operating
//! system, and Odoo.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::daemon::{MergeRequestRequest, StageMove, StageMoveRequest, TaskBackend, TaskDetail};
use crate::scan::{ProcessRow, ProcessSource};

/// A scripted `ps`/`lsof`. Unlike the scanner suite's fake this one is `Send`,
/// because the engine's loop thread holds the scanner — and because the ticks
/// that have to be made slow or explosive are what the re-entrancy and
/// panic-resilience tests are about.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeProcesses {
    rows: Arc<Mutex<Vec<ProcessRow>>>,
    cwds: Arc<Mutex<HashMap<u32, String>>>,
    environ: Arc<Mutex<HashMap<u32, String>>>,
    delay: Arc<Mutex<Duration>>,
    explode: Arc<AtomicBool>,
    listed: Arc<AtomicU32>,
}

impl FakeProcesses {
    pub(crate) fn new() -> Self {
        FakeProcesses::default()
    }

    /// A `claude` started a minute ago in `cwd` — comfortably inside the
    /// pairing window, whenever the test happens to run.
    pub(crate) fn add(&self, pid: u32, cwd: &str) -> &Self {
        let started = chrono::Local::now() - chrono::Duration::seconds(60);
        self.rows.lock().unwrap().push(ProcessRow {
            pid,
            tty: Some(format!("ttys{pid:03}")),
            lstart: started.format("%a %b %e %H:%M:%S %Y").to_string(),
            comm: "claude".to_string(),
        });
        self.cwds.lock().unwrap().insert(pid, cwd.to_string());
        self
    }

    /// The task this process was launched for, as `ps -E` reports it. Pairing
    /// strategy 1b matches it against the task each transcript names, which is
    /// exact and needs no timing at all.
    pub(crate) fn launched_for(&self, pid: u32, task_id: i64) -> &Self {
        self.environ
            .lock()
            .unwrap()
            .insert(pid, format!("claude CLAUDE_SESSIONS_TASK_ID={task_id}"));
        self
    }

    /// A process that is not Claude, such as the daemon itself. Every real
    /// listing has one, which is what makes a listing with no `claude` rows a
    /// readable answer rather than a failed `ps`.
    pub(crate) fn add_bystander(&self, pid: u32) -> &Self {
        self.rows.lock().unwrap().push(ProcessRow {
            pid,
            tty: None,
            lstart: String::new(),
            comm: "claude-sessions".to_string(),
        });
        self
    }

    pub(crate) fn remove(&self, pid: u32) -> &Self {
        self.rows.lock().unwrap().retain(|row| row.pid != pid);
        self.cwds.lock().unwrap().remove(&pid);
        self.environ.lock().unwrap().remove(&pid);
        self
    }

    /// Make a tick take long enough for another one to arrive during it.
    pub(crate) fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    /// Make the next scan panic, as a transcript from a future Claude Code
    /// release one day will.
    pub(crate) fn set_explode(&self, explode: bool) {
        self.explode.store(explode, Ordering::SeqCst);
    }

    pub(crate) fn listed(&self) -> u32 {
        self.listed.load(Ordering::SeqCst)
    }
}

impl ProcessSource for FakeProcesses {
    fn list(&self) -> Vec<ProcessRow> {
        self.listed.fetch_add(1, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        assert!(
            !self.explode.load(Ordering::SeqCst),
            "scripted tick failure"
        );
        self.rows.lock().unwrap().clone()
    }

    fn cwds(&self, pids: &[u32]) -> HashMap<u32, String> {
        let cwds = self.cwds.lock().unwrap();
        pids.iter()
            .filter_map(|pid| cwds.get(pid).map(|cwd| (*pid, cwd.clone())))
            .collect()
    }

    fn argv(&self, pids: &[u32]) -> HashMap<u32, String> {
        pids.iter()
            .map(|pid| (*pid, "claude".to_string()))
            .collect()
    }

    fn environ(&self, pids: &[u32]) -> HashMap<u32, String> {
        let environ = self.environ.lock().unwrap();
        pids.iter()
            .map(|pid| {
                (
                    *pid,
                    environ
                        .get(pid)
                        .cloned()
                        .unwrap_or_else(|| "claude".to_string()),
                )
            })
            .collect()
    }
}

// --- the task backend -------------------------------------------------------

/// Records what the completion flow asked of Odoo and GitLab, and answers with
/// whatever the test scripted. The real implementations arrive in Phases 9b
/// and 10.
#[derive(Debug, Clone, Default)]
pub(crate) struct RecordingBackend {
    pub(crate) calls: Arc<Mutex<Vec<BackendCall>>>,
    pub(crate) mr_url: Arc<Mutex<Option<String>>>,
    pub(crate) stage: Arc<Mutex<Option<StageMove>>>,
    pub(crate) detail: Arc<Mutex<TaskDetail>>,
    /// Makes the next `set_task_state` fail, for the "Odoo refused one of them"
    /// path the deploy report has to survive.
    pub(crate) state_error: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackendCall {
    Detail(i64),
    MergeRequest(MergeRequestRequest),
    MoveStage(StageMoveRequest),
    Comment { task_id: i64, html: String },
    State { task_id: i64, state: String },
}

impl RecordingBackend {
    pub(crate) fn calls(&self) -> Vec<BackendCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Every task this backend was asked to set the state of, in order.
    pub(crate) fn state_writes(&self) -> Vec<(i64, String)> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                BackendCall::State { task_id, state } => Some((task_id, state)),
                _ => None,
            })
            .collect()
    }

    /// The HTML posted to a task's chatter, if any.
    pub(crate) fn comment(&self) -> Option<String> {
        self.calls().into_iter().find_map(|call| match call {
            BackendCall::Comment { html, .. } => Some(html),
            _ => None,
        })
    }
}

impl TaskBackend for RecordingBackend {
    fn task_detail(&self, task_id: i64) -> Result<TaskDetail, String> {
        self.calls
            .lock()
            .unwrap()
            .push(BackendCall::Detail(task_id));
        Ok(self.detail.lock().unwrap().clone())
    }

    fn ensure_merge_request(
        &self,
        request: &MergeRequestRequest,
    ) -> Result<Option<String>, String> {
        self.calls
            .lock()
            .unwrap()
            .push(BackendCall::MergeRequest(request.clone()));
        Ok(self.mr_url.lock().unwrap().clone())
    }

    fn move_to_stage(&self, request: &StageMoveRequest) -> Result<StageMove, String> {
        self.calls
            .lock()
            .unwrap()
            .push(BackendCall::MoveStage(request.clone()));
        Ok(self
            .stage
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(StageMove::Disabled))
    }

    fn post_comment(&self, task_id: i64, html: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(BackendCall::Comment {
            task_id,
            html: html.to_string(),
        });
        Ok(())
    }

    fn set_task_state(&self, task_id: i64, state: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(BackendCall::State {
            task_id,
            state: state.to_string(),
        });
        if let Some(error) = self.state_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(())
    }
}
