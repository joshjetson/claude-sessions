//! The Odoo client: authentication, `execute_kw`, and the caches around them.
//!
//! Credentials arrive already resolved (config beats `ODOO_*` per field — see
//! [`crate::config::ConfigHandle::odoo_creds`]) rather than being re-read from
//! disk on every call the way the Node version did.
//!
//! Three things the Node client did not do:
//!
//! * every request carries a timeout;
//! * the cached uid can be invalidated, so an expired session recovers without
//!   restarting the dashboard;
//! * the metadata caches (projects, tags, stages) can be invalidated too — in
//!   Node they were populated once and never refreshed, so a renamed stage
//!   stayed wrong until the process was restarted.

mod fetch;
mod queries;
pub mod records;
mod rpc;
mod stages;

pub use fetch::{FetchBoardOptions, QaStageTask};
pub use records::{Blocker, OdooProject, TaskDetail, TaskGitlab, TASK_FIELDS};
pub use rpc::{HttpTransport, OdooError, Result, Transport, DEFAULT_TIMEOUT};
pub use stages::{
    resolve_stage, task_state, StageKind, StageMatch, StageRecord, CLOSED_STATES,
    DEFAULT_DONE_STAGES, DEFAULT_IN_PROGRESS_STAGES,
};

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::types::OdooCreds;

/// Stage metadata, keyed by stage id: the board needs the sequence to order
/// columns that several projects share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageMeta {
    pub name: String,
    pub sequence: i64,
}

#[derive(Debug, Default)]
struct Caches {
    uid: Option<i64>,
    projects: Option<Vec<OdooProject>>,
    tags: Option<HashMap<i64, String>>,
    stages: Option<HashMap<i64, StageMeta>>,
}

/// A connection to one Odoo instance.
///
/// Cheap to construct and safe to share: the caches sit behind a mutex that is
/// never held across a request.
pub struct OdooClient {
    creds: OdooCreds,
    transport: Box<dyn Transport>,
    caches: Mutex<Caches>,
}

impl OdooClient {
    pub fn new(creds: OdooCreds) -> Self {
        OdooClient::with_timeout(creds, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(creds: OdooCreds, timeout: Duration) -> Self {
        OdooClient::with_transport(creds, Box::new(HttpTransport::new(timeout)))
    }

    /// Used by the tests (which point a real [`HttpTransport`] at a loopback
    /// stub) and available to anything that needs to record or replay calls.
    pub fn with_transport(creds: OdooCreds, transport: Box<dyn Transport>) -> Self {
        OdooClient {
            creds,
            transport,
            caches: Mutex::new(Caches::default()),
        }
    }

    pub fn creds(&self) -> &OdooCreds {
        &self.creds
    }

    /// All four fields present. Checked before a refresh so a half-configured
    /// install says so instead of failing one request at a time.
    pub fn has_credentials(&self) -> bool {
        self.creds.is_complete()
    }

    /// Replace the credentials — after the config file changed, say. Everything
    /// cached under the old ones is dropped.
    pub fn set_credentials(&mut self, creds: OdooCreds) {
        self.creds = creds;
        self.invalidate_all();
    }

    /// Forget the uid. Call this when a request fails authentication: the next
    /// one re-authenticates instead of replaying a session Odoo has forgotten.
    pub fn invalidate_auth(&self) {
        self.with_caches(|caches| caches.uid = None);
    }

    /// Forget projects, tags and stages — the values that go stale when
    /// somebody renames a stage or adds a project. The refresh loop calls this;
    /// Node had no way to.
    pub fn invalidate_metadata(&self) {
        self.with_caches(|caches| {
            caches.projects = None;
            caches.tags = None;
            caches.stages = None;
        });
    }

    pub fn invalidate_all(&self) {
        self.with_caches(|caches| *caches = Caches::default());
    }

    /// The uid for the configured user, authenticating once and caching it.
    pub fn authenticate(&self) -> Result<i64> {
        let missing = self.missing_credentials();
        if !missing.is_empty() {
            return Err(OdooError::MissingCredentials(missing));
        }
        if let Some(uid) = self.with_caches(|caches| caches.uid) {
            return Ok(uid);
        }

        let result = self.rpc(
            "common",
            "authenticate",
            vec![
                json!(self.creds.db),
                json!(self.creds.user),
                json!(self.creds.password),
                json!({}),
            ],
        )?;
        // Odoo answers a rejected login with `false`, not an error object.
        let uid = result.as_i64().filter(|uid| *uid > 0).ok_or_else(|| {
            // Say what this process actually saw: the usual cause is a shell
            // that exported stale credentials before the dashboard started.
            OdooError::AuthRejected(format!(
                "user={} db={}… pw={} chars",
                self.creds.user,
                self.creds.db.chars().take(16).collect::<String>(),
                self.creds.password.len()
            ))
        })?;
        self.with_caches(|caches| caches.uid = Some(uid));
        Ok(uid)
    }

    /// `object.execute_kw` — every model call goes through here.
    pub fn execute_kw(
        &self,
        model: &str,
        method: &str,
        args: Vec<Value>,
        kwargs: Value,
    ) -> Result<Value> {
        let uid = self.authenticate()?;
        self.rpc(
            "object",
            "execute_kw",
            vec![
                json!(self.creds.db),
                json!(uid),
                json!(self.creds.password),
                json!(model),
                json!(method),
                Value::Array(args),
                kwargs,
            ],
        )
    }

    /// A web URL that opens the task form whatever the action menus do.
    pub fn task_url(&self, task_id: i64) -> String {
        format!(
            "{}/web#id={task_id}&model=project.task&view_type=form",
            self.creds.url
        )
    }

    fn rpc(&self, service: &str, method: &str, args: Vec<Value>) -> Result<Value> {
        rpc::call(
            self.transport.as_ref(),
            &self.creds.url,
            service,
            method,
            args,
        )
    }

    fn missing_credentials(&self) -> Vec<&'static str> {
        [
            ("url", &self.creds.url),
            ("db", &self.creds.db),
            ("user", &self.creds.user),
            ("password", &self.creds.password),
        ]
        .into_iter()
        .filter(|(_, value)| value.is_empty())
        .map(|(name, _)| name)
        .collect()
    }

    /// One place that recovers from a poisoned cache mutex: a panic elsewhere
    /// must not make the board permanently unreadable.
    fn with_caches<T>(&self, f: impl FnOnce(&mut Caches) -> T) -> T {
        let mut caches = self.caches.lock().unwrap_or_else(|err| err.into_inner());
        f(&mut caches)
    }
}

#[cfg(test)]
pub(crate) mod tests;
