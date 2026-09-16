//! The values a pipeline run is parameterised by, and the small helpers every
//! prompt fragment shares.
//!
//! One type rather than a bag of strings: the fragments are data, so the thing
//! they read has to be a fixed shape, and `{{var}}` interpolation in a project
//! override resolves against the same values the built-in steps see.

use std::collections::BTreeMap;

/// The binary the spawned agent calls back with.
///
/// The Node app baked absolute paths to seven helper scripts into every prompt
/// (`node /…/bin/done.js 5944 …`). One binary with subcommands replaces them,
/// so the prompt names a command that is on the agent's PATH instead of a file
/// path that only existed on the machine that wrote the prompt.
pub const BIN: &str = "claude-sessions";

/// `claude-sessions done <id> --summary-file <path>` — the completion signal.
pub fn done_command(task_id: i64, summary_file: &str) -> String {
    format!("{BIN} done {task_id} --summary-file {summary_file}")
}

/// `claude-sessions blocked <id> --questions "…"` — the readiness-gate stop.
pub fn blocked_command(task_id: i64, questions: &str) -> String {
    format!("{BIN} blocked {task_id} --questions \"{questions}\"")
}

/// `claude-sessions notify --title "…" --message "…"` — reaching the dashboard.
pub fn notify_command(title: &str, message: &str) -> String {
    format!("{BIN} notify --title \"{title}\" --message \"{message}\"")
}

/// The merge request a conflict-resolution run is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeRequestVars {
    pub iid: i64,
    pub url: String,
    pub source_branch: String,
    pub target_branch: String,
}

/// Everything a prompt fragment may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptVars {
    pub task_id: i64,
    /// The Odoo task URL. Load-bearing: sessions are linked back to tasks by
    /// finding this in the transcript, not by the environment variable.
    pub url: String,
    /// Free text typed at launch. Collapsed to one line when rendered.
    pub extra_context: String,
    /// An archived transcript of earlier work on this task, when there is one.
    pub archive_path: Option<String>,
    /// Where the agent writes its plain-English write-up.
    pub summary_file: String,
    /// How the prompt describes the branch an MR should target.
    pub branch_instruction: String,
    /// `None` means "not supplied" — a project's `vars.targetBranch` then wins,
    /// which is what the Node version's `undefined` check did.
    pub target_branch: Option<String>,
    pub repo_path: Option<String>,
    /// Conflict pipeline only.
    pub mr: Option<MergeRequestVars>,
    /// Conflict pipeline only: the session that wrote the branch is being
    /// resumed, so it already knows why every hunk looks the way it does.
    pub resumed: bool,
    /// Anything else a caller wants interpolatable.
    pub extras: BTreeMap<String, String>,
}

impl PromptVars {
    pub fn new(task_id: i64, url: impl Into<String>) -> Self {
        PromptVars {
            task_id,
            url: url.into(),
            ..PromptVars::default()
        }
    }

    /// The run's value for an interpolation key, if it supplied one.
    fn supplied(&self, key: &str) -> Option<String> {
        let mr = |pick: fn(&MergeRequestVars) -> String| self.mr.as_ref().map(pick);
        match key {
            "taskId" => Some(self.task_id.to_string()),
            "url" => Some(self.url.clone()),
            "extraContext" => Some(self.extra_context.clone()),
            "archivePath" => self.archive_path.clone(),
            "summaryFile" => Some(self.summary_file.clone()),
            "branchInstruction" => Some(self.branch_instruction.clone()),
            "targetBranch" => self.target_branch.clone(),
            "repoPath" => self.repo_path.clone(),
            "mrIid" => mr(|mr| mr.iid.to_string()),
            "mrUrl" => mr(|mr| mr.url.clone()),
            "mrSourceBranch" => mr(|mr| mr.source_branch.clone()),
            "mrTargetBranch" => mr(|mr| mr.target_branch.clone()),
            _ => self.extras.get(key).cloned(),
        }
    }

    /// The one-line "honor this above generic assumptions" sentence, or nothing.
    ///
    /// Four Node pipelines built this string separately; it is written once
    /// here so a change to the wording cannot apply to three of them.
    pub fn extra_context_sentence(&self) -> String {
        let collapsed = collapse_whitespace(&self.extra_context);
        if collapsed.is_empty() {
            return String::new();
        }
        format!(
            " IMPORTANT extra context from the user — honor this above generic assumptions: {collapsed}."
        )
    }

    pub fn merge_request(&self) -> MergeRequestVars {
        self.mr.clone().unwrap_or_default()
    }
}

/// Which wording the branch instruction falls back to when a project has no
/// configured target branch. The two built-in pipelines differ here only in
/// phrasing, which is preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchFallback {
    /// "the repo's default branch, usually development" — the task pipeline.
    Development,
    /// "the default branch" — the revision pipeline.
    Default,
}

/// How the prompt describes the branch an MR should target.
pub fn branch_instruction(target_branch: Option<&str>, fallback: BranchFallback) -> String {
    match target_branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => {
            format!("the `{branch}` branch (this project targets `{branch}`, NOT development)")
        }
        None => match fallback {
            BranchFallback::Development => {
                "the repo's default branch, usually development".to_string()
            }
            BranchFallback::Default => "the default branch".to_string(),
        },
    }
}

/// `{{name}}` in an override's prompt/run/append, filled from the run's
/// variables with the project's `vars` as defaults. An unknown key is left
/// visible rather than blanked, so a typo shows up in the prompt instead of
/// silently deleting a sentence's subject.
pub fn interpolate(
    text: &str,
    vars: &PromptVars,
    project_vars: &BTreeMap<String, String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            break;
        };
        let end = start + end;
        let key = rest[start + 2..end].trim();
        out.push_str(&rest[..start]);
        match lookup(key, vars, project_vars) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..end + 2]),
        }
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// A run-time value wins over the project default; nothing found leaves the
/// placeholder in place.
fn lookup(key: &str, vars: &PromptVars, project_vars: &BTreeMap<String, String>) -> Option<String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    vars.supplied(key)
        .or_else(|| project_vars.get(key).cloned())
}

/// Whitespace runs collapse to single spaces — typed context is often pasted
/// across several lines, and a prompt is one paragraph.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
