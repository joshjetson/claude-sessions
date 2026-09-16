//! What the worker thread is handed: a launch, a send, or an archive lookup.
//!
//! Everything a prompt needs that comes from config is resolved HERE, on the
//! UI thread that owns the config, and travels in the spec. The worker holds no
//! configuration of its own, so there is no second copy to go stale after the
//! target branch is edited in a dialog — and a spec is a plain value, which is
//! what makes the launch flow assertable without a terminal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::daemon::StageMoveRequest;
use crate::pipeline::{
    branch_instruction, resolve_pipeline, PromptVars, ResolvedPipeline, UnknownPipeline,
};
use crate::term::SessionRef;

use super::launch::LaunchKind;

/// The run-specific half of [`PromptVars`], resolved once and shared by every
/// path that assembles a prompt.
///
/// Written once because the Node app built these variables in four places
/// (`launchTaskSession`, `launchResumeSession`, `launchQaSession`,
/// `sendRevisionToSession`) and they had drifted: two passed `archivePath`, one
/// passed `targetBranch` and one did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptContext {
    pub task_id: i64,
    /// The Odoo task URL. Load-bearing beyond being a link: the scanner
    /// recovers a session's task by finding it in the transcript head.
    pub task_url: String,
    pub extra_context: String,
    /// Where the agent writes its plain-English write-up.
    pub summary_file: String,
    pub target_branch: Option<String>,
    /// `taskName` / `projectName` / `stageName`, for a project override's
    /// `{{...}}` placeholders.
    pub extras: BTreeMap<String, String>,
}

impl PromptContext {
    pub fn vars(&self, repo: &str, kind: &LaunchKind, archive_path: Option<String>) -> PromptVars {
        PromptVars {
            extra_context: self.extra_context.clone(),
            archive_path,
            summary_file: self.summary_file.clone(),
            branch_instruction: branch_instruction(
                self.target_branch.as_deref(),
                kind.branch_fallback(),
            ),
            target_branch: self.target_branch.clone(),
            repo_path: Some(repo.to_string()),
            extras: self.extras.clone(),
            ..PromptVars::new(self.task_id, self.task_url.clone())
        }
    }

    fn pipeline(
        &self,
        kind: &LaunchKind,
        repo: &str,
    ) -> Result<Option<ResolvedPipeline>, UnknownPipeline> {
        match kind.pipeline_id() {
            None => Ok(None),
            Some(id) => resolve_pipeline(id, Some(Path::new(repo))).map(Some),
        }
    }

    /// The assembled prompt, or `None` for a launch that deliberately sends
    /// nothing.
    pub fn prompt(
        &self,
        kind: &LaunchKind,
        repo: &str,
        archive_path: Option<String>,
    ) -> Result<Option<String>, UnknownPipeline> {
        Ok(self
            .pipeline(kind, repo)?
            .map(|resolved| resolved.build_prompt(&self.vars(repo, kind, archive_path))))
    }
}

/// A launch, fully resolved. The worker writes the prompt file, opens the
/// terminal and asks Odoo to move the stage — it decides nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub task_id: i64,
    pub cwd: String,
    pub flags: String,
    /// `None` opens the session at an empty input. The resume-conversation path
    /// is the only one that does: everything else hands the agent instructions
    /// and expects it to act on them.
    pub prompt: Option<String>,
    pub title: String,
    /// The best-effort work-ward move that rides along, when this kind of
    /// launch means work is starting.
    pub stage_move: Option<StageMoveRequest>,
    /// Session ids that existed when the launch went out, so the linker can
    /// tell the new one apart from them.
    pub known_session_ids: Vec<String>,
    /// Shown once the terminal is open.
    pub say: String,
}

/// Handing a revision to a session that is already open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendSpec {
    pub task_id: i64,
    pub session: SessionRef,
    pub session_id: String,
    pub prompt: String,
    pub stage_move: Option<StageMoveRequest>,
}

/// Picking an archived conversation back up.
///
/// The archive lookup is file I/O with a self-healing copy in it (it restores
/// the transcript into Claude Code's own project directory, which is what
/// `--resume` reads), so it happens on the worker. Everything the prompt needs
/// from config is already resolved in `prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    pub prompt: PromptContext,
    /// `false` sends no prompt at all and moves no stage: reading a
    /// conversation is not starting work on it (test-pinned).
    pub revision: bool,
    /// Where the dashboard last saw this task worked, for when the archive
    /// record names no folder of its own.
    pub link_cwd: String,
    /// The work-ward move a revision makes. `repo_path` is filled in by the
    /// worker, which is the first thing to learn where the session actually is.
    pub stage_move: Option<StageMoveRequest>,
    pub known_session_ids: Vec<String>,
}

impl ResumeRequest {
    /// The spec, once the archive has answered with a session and a folder.
    pub fn spec(
        &self,
        session_id: &str,
        cwd: &str,
        archive_path: Option<String>,
    ) -> Result<LaunchSpec, UnknownPipeline> {
        let kind = if self.revision {
            LaunchKind::Revision {
                session_id: session_id.to_string(),
            }
        } else {
            LaunchKind::Conversation {
                session_id: session_id.to_string(),
            }
        };
        let task_id = self.prompt.task_id;
        Ok(LaunchSpec {
            task_id,
            cwd: cwd.to_string(),
            flags: kind.flags(),
            prompt: self.prompt.prompt(&kind, cwd, archive_path)?,
            title: format!("task-{task_id}"),
            stage_move: self.stage_move.clone().map(|mut request| {
                request.repo_path = Some(PathBuf::from(cwd));
                request
            }),
            known_session_ids: self.known_session_ids.clone(),
            say: if self.revision {
                format!(
                    "Resumed #{task_id} ({}) with revision notes.",
                    short(session_id)
                )
            } else {
                // Said explicitly because it is the surprising part: this is
                // the one path that asks the agent for nothing.
                format!(
                    "Resuming #{task_id} ({}) — no prompt sent.",
                    short(session_id)
                )
            },
        })
    }
}

pub fn short(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}
