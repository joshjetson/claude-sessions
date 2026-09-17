//! GitLab through the `glab` CLI — not its REST API.
//!
//! Two reasons the Deploy tab reads GitLab at all rather than trusting Odoo:
//! the task's `gitlab_merge_request_state` field only updates when the
//! integration syncs, so it goes stale the moment somebody merges; and the
//! task's assignee is routinely not the merge request's author, so the task
//! record cannot answer "whose MR is this".
//!
//! Two reasons it is the CLI and not HTTPS: `glab` already holds the user's
//! credentials (this crate never asks for a token, never stores one and never
//! sees one), and it is the same binary the user merges with by hand, so the
//! dashboard and the terminal agree about what is mergeable.
//!
//! Layering matches [`crate::term`]: [`argv`] builds every command as a pure
//! function, [`parse`] turns the JSON into the domain types, and this file is
//! the only part that needs a process — behind a [`Runner`], so the retry rules
//! below are tested with a recorded script and no `glab` anywhere.

pub mod argv;
pub mod parse;

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::config::ConfigHandle;
use crate::term::{Exec, ExecRequest, Runner, SpawnPolicy};
use crate::types::MergeRequest;

pub use argv::{
    can_open_mr_from, encode_uri_component, parse_mr_url, project_from_ref, MergeOptions, MrRef,
    MrScope, DETACHED, FALLBACK_TARGET, GIT, GLAB, PROTECTED_BRANCHES,
};
pub use parse::{is_pending_check, to_merge_request, to_open_mrs, OpenMr, PENDING_STATUSES};

/// Every `glab` call but the merge gets this long. The Node wrapper's value.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(25);
/// Merging waits on GitLab's own merge job, which can take a while.
pub const MERGE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait before re-reading an MR whose mergeability check is still
/// running. See [`Gitlab::fetch_mr`].
pub const RECHECK_DELAY: Duration = Duration::from_millis(1200);

/// The environment variable `glab` takes its server from.
pub const HOST_ENV: &str = "GITLAB_HOST";

/// What a `glab` call can go wrong with. Always a sentence for a user: these
/// reach the Deploy tab verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitlabError {
    /// No `gitlabHost` in the config and no `GITLAB_HOST` in the environment.
    /// A hint, never a crash: an install with no GitLab at all is supported,
    /// and every GitLab feature simply says this instead of working.
    NotConfigured,
    /// `glab` ran and failed — its first line of stderr, which is GitLab's own
    /// reason (open pipeline, conflicts, approvals missing).
    Failed(String),
    /// `glab` answered something that is not JSON.
    Unparseable,
}

impl fmt::Display for GitlabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitlabError::NotConfigured => f.write_str(
                "No GitLab host configured — set \"gitlabHost\" in \
                 ~/.claude-sessions.json or export GITLAB_HOST.",
            ),
            GitlabError::Failed(message) => f.write_str(message),
            GitlabError::Unparseable => f.write_str("could not parse glab output"),
        }
    }
}

impl std::error::Error for GitlabError {}

pub type Result<T> = std::result::Result<T, GitlabError>;

/// The `glab` wrapper.
///
/// Cheap to clone and safe to share: it owns a host string and a runner, and
/// holds no connection of its own.
#[derive(Clone)]
pub struct Gitlab {
    host: Option<String>,
    runner: Arc<dyn Runner>,
    /// Injected so the retry test does not sleep 1.2 seconds.
    recheck_delay: Duration,
}

impl Gitlab {
    /// `host` is `None` when nothing is configured, which turns every call into
    /// [`GitlabError::NotConfigured`] rather than a failed spawn.
    pub fn new(host: Option<String>, runner: Arc<dyn Runner>) -> Self {
        Gitlab {
            host,
            runner,
            recheck_delay: RECHECK_DELAY,
        }
    }

    /// The wrapper for this install: the configured host (env `GITLAB_HOST`
    /// beats `gitlabHost`, as everywhere else) and a real [`Exec`] behind the
    /// spawn policy this process runs under.
    ///
    /// `None` for the host is a supported configuration, not a failure — see
    /// [`GitlabError::NotConfigured`].
    pub fn for_config(config: &ConfigHandle, policy: SpawnPolicy) -> Gitlab {
        Gitlab::new(
            config.gitlab_host().map(str::to_string),
            Arc::new(Exec::new(policy)) as Arc<dyn Runner>,
        )
    }

    pub fn with_recheck_delay(mut self, delay: Duration) -> Self {
        self.recheck_delay = delay;
        self
    }

    /// Whether GitLab features are available at all.
    pub fn is_configured(&self) -> bool {
        self.host.is_some()
    }

    /// One `glab` invocation. `cwd` matters only for `mr create`, which reads
    /// the repository's remote to decide which project it is opening against.
    fn glab(&self, args: &[String], timeout: Duration, cwd: Option<&Path>) -> Result<String> {
        let Some(host) = &self.host else {
            return Err(GitlabError::NotConfigured);
        };
        let env = [(HOST_ENV.to_string(), host.clone())];
        let output = self.runner.run_request(
            &ExecRequest::new(GLAB, args, timeout)
                .in_dir(cwd)
                .with_env(&env),
        );
        if output.ok {
            return Ok(output.stdout);
        }
        Err(GitlabError::Failed(first_line(&output.failure_message())))
    }

    /// `git`, for the two steps that precede opening a merge request. Not
    /// gated on the GitLab host: a repository's branch is readable whether or
    /// not this install talks to GitLab.
    fn git(&self, args: &[String]) -> Result<String> {
        let output = self
            .runner
            .run_request(&ExecRequest::new(GIT, args, DEFAULT_TIMEOUT));
        if output.ok {
            return Ok(output.stdout);
        }
        Err(GitlabError::Failed(first_line(&output.failure_message())))
    }

    fn api(&self, path: &str) -> Result<Value> {
        let stdout = self.glab(&argv::api_args(path), DEFAULT_TIMEOUT, None)?;
        serde_json::from_str(&stdout).map_err(|_| GitlabError::Unparseable)
    }

    /// Live state for one merge request.
    ///
    /// Reading an MR is what KICKS GitLab's mergeability check, so the first
    /// answer is routinely `checking`/`unchecked`/`preparing`. One short retry
    /// turns that into a real verdict instead of a permanently indefinite badge
    /// — and it is only one, because the read itself is what started the check
    /// and a second wait would not make GitLab faster.
    pub fn fetch_mr(&self, project_path: &str, iid: i64) -> Result<MergeRequest> {
        let path = argv::mr_api_path(project_path, iid);
        let record = self.api(&path)?;
        let mut mr = parse::to_merge_request(&record);
        if is_pending_check(&mr.merge_status) {
            thread::sleep(self.recheck_delay);
            // A failed retry keeps the first answer: "checking" is a truthful
            // badge, and an error here would throw away a usable record.
            if let Ok(record) = self.api(&path) {
                mr = parse::to_merge_request(&record);
            }
        }
        Ok(mr)
    }

    /// The current user's OPEN merge requests, newest-updated first.
    pub fn fetch_open_mrs(&self, scope: MrScope) -> Result<Vec<OpenMr>> {
        let body = self.api(&argv::open_mrs_api_path(scope))?;
        Ok(parse::to_open_mrs(&body))
    }

    /// Merge one merge request.
    ///
    /// The error is GitLab's own sentence, unedited, because it names the
    /// actual blocker — an open pipeline, missing approvals, a conflict — and
    /// the dashboard has nothing better to say than GitLab does.
    pub fn merge_mr(&self, project_path: &str, iid: i64, options: MergeOptions) -> Result<String> {
        self.glab(
            &argv::merge_args(project_path, iid, options),
            MERGE_TIMEOUT,
            None,
        )
    }

    /// The repository's current branch, or `None` on a detached HEAD.
    pub fn current_branch(&self, cwd: &Path) -> Option<String> {
        let branch = self.git(&argv::current_branch_args(cwd)).ok()?;
        let branch = branch.trim().to_string();
        (!branch.is_empty() && branch != DETACHED).then_some(branch)
    }

    /// Push the branch and open a merge request for it, returning its URL.
    ///
    /// The push is not optional: `glab mr create` against a branch the remote
    /// has never seen fails with a message about the source branch, which reads
    /// as a permissions problem rather than the missing push it is.
    pub fn create_mr(&self, cwd: &Path, branch: &str, target: &str) -> Result<Option<String>> {
        self.git(&argv::push_args(cwd, branch))?;
        let out = self.glab(
            &argv::create_mr_args(branch, target),
            DEFAULT_TIMEOUT,
            Some(cwd),
        )?;
        Ok(parse::first_url(&out))
    }
}

/// `glab` reports one failure over several lines; the first is the reason.
fn first_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("glab failed")
        .to_string()
}

#[cfg(test)]
pub(crate) mod tests;
