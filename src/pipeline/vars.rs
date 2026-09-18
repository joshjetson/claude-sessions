//! The values a pipeline run is parameterised by, and the small helpers every
//! prompt fragment shares.
//!
//! One type rather than a bag of strings: the fragments are data, so the thing
//! they read has to be a fixed shape, and `{{var}}` interpolation in a project
//! override resolves against the same values the built-in steps see.

use std::collections::BTreeMap;

/// The name to fall back on when this process cannot say where it lives.
pub const BIN_NAME: &str = "claude-sessions";

/// The binary the spawned agent calls back with — an ABSOLUTE path.
///
/// It used to be the bare name `claude-sessions`, resolved against whatever
/// PATH the spawned session happened to inherit. That is how the QA run's
/// reporting went silent: a Node build of this tool installs a
/// `claude-sessions` of its own, and on a machine where it wins PATH the bare
/// name reaches THAT binary, where `notify` is not a subcommand — it opens the
/// session-manager TUI. So every `claude-sessions notify` an agent ran took
/// over its terminal and never returned. Nothing was written, nothing was
/// escalated, and the agents ended up running `pkill -f "claude-sessions
/// notify"` to clear the wreckage, which killed each other's in-flight calls
/// too.
///
/// The prompt now names the binary that BUILT it. There is no resolution step
/// left to get wrong, and an agent cannot be handed a different implementation
/// of the same name.
///
/// Resolved once: `current_exe` is a syscall, and the answer cannot change
/// while this process lives.
/// Overrides [`bin`]. Set by the golden-master tests, which cannot depend on
/// where a test binary happens to live, and available to pin the path by hand.
pub const BIN_ENV: &str = "CLAUDE_SESSIONS_BIN";

pub fn bin() -> &'static str {
    static BIN_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BIN_PATH.get_or_init(|| {
        if let Ok(pinned) = std::env::var(BIN_ENV) {
            if !pinned.trim().is_empty() {
                return pinned;
            }
        }
        std::env::current_exe()
            .ok()
            // A path with a space in it would break the command line the agent
            // is told to run, and quoting it here would be quoted again by
            // whatever composes the prompt. The bare name is wrong less often
            // than a mangled path.
            .filter(|path| !path.to_string_lossy().contains(char::is_whitespace))
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| BIN_NAME.to_string())
    })
}

/// `claude-sessions done <id> --summary-file <path>` — the completion signal.
pub fn done_command(task_id: i64, summary_file: &str) -> String {
    format!("{} done {task_id} --summary-file {summary_file}", bin())
}

/// `claude-sessions blocked <id> --questions "…"` — the readiness-gate stop.
pub fn blocked_command(task_id: i64, questions: &str) -> String {
    format!("{} blocked {task_id} --questions \"{questions}\"", bin())
}

/// `claude-sessions notify --title "…" --message "…"` — reaching the dashboard.
pub fn notify_command(title: &str, message: &str) -> String {
    format!(
        "{} notify --title \"{title}\" --message \"{message}\"",
        bin()
    )
}

/// `claude-sessions qa-shadow …` — recording what a coordinator WOULD answer.
///
/// Called before the coordinator acts, never after. The store refuses to
/// overwrite, and that refusal is what makes the record worth keeping.
pub fn qa_shadow_command(run_id: &str, task_id: i64) -> String {
    format!(
        "{} qa-shadow --run \"{run_id}\" --task {task_id} \
         --question \"<their question>\" --would-answer \"<your answer>\" \
         --confidence high|medium|low",
        bin()
    )
}

/// `claude-sessions qa-answer …` — delivering an answer to a QA session.
///
/// The daemon decides whether it may be delivered: a question may be answered,
/// a verdict checkpoint never may be, whatever this command is told.
pub fn qa_answer_command(task_id: i64) -> String {
    format!(
        "{} qa-answer --task {task_id} --answer \"<your answer>\"",
        bin()
    )
}

/// `claude-sessions notify … --kind <kind>` — an escalation that says what it
/// is, so a run can count what is blocked rather than what is merely loud.
pub fn notify_kind_command(title: &str, message: &str, level: &str, kind: &str) -> String {
    format!(
        "{} notify --title \"{title}\" --message \"{message}\" --level {level} --kind {kind}",
        bin()
    )
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
