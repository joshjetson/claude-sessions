//! Ported from the Node app's `test/pipeline.test.js` and `test/skills.test.js`.
//!
//! The most important tests here are the golden masters: the prompt assembled
//! from the definitions must stay byte-identical to the one the dashboard sent
//! before the pipelines became data, apart from the substitutions the
//! single-binary decision forces (the helper scripts became
//! `claude-sessions <subcommand>`, and the summary file moved out of `/tmp`).

mod definitions;
mod golden;
mod overrides;
mod qa_run;
mod skills;
mod stage_steps;
mod template;

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::{project::project_pipeline_path, PromptVars};

pub(crate) const TASK_ID: i64 = 5944;
pub(crate) const URL: &str = "https://odoo/web#id=5944&model=project.task&view_type=form";
/// The port's summary path: `Paths::task_summary_file`, pinned as a literal so
/// the golden masters do not depend on where a test's temp directory landed.
/// `paths::tests` pins that the method really produces this shape.
pub(crate) const SUMMARY: &str = "/runtime/summaries/task-5944-summary.md";
pub(crate) const BRANCH: &str = "the `main` branch (this project targets `main`, NOT development)";

/// The variables every test builds its prompt from — the Node fixture, with the
/// script paths gone because the prompts now name subcommands.
/// A built prompt with this build's binary path normalised to the bare name the
/// pinned prompts are written with. See `golden::prompt` for why.
pub(crate) fn normalise_bin(prompt: &str) -> String {
    prompt.replace(crate::pipeline::vars::bin(), "claude-sessions")
}

pub(crate) fn vars() -> PromptVars {
    PromptVars {
        task_id: TASK_ID,
        url: URL.to_string(),
        extra_context: String::new(),
        archive_path: None,
        summary_file: SUMMARY.to_string(),
        branch_instruction: BRANCH.to_string(),
        target_branch: Some("main".to_string()),
        ..PromptVars::default()
    }
}

/// A throwaway repo, optionally carrying a `pipeline.json`.
pub(crate) struct Repo {
    dir: TempDir,
}

impl Repo {
    pub(crate) fn empty() -> Self {
        Repo {
            dir: tempfile::tempdir().expect("temp repo"),
        }
    }

    pub(crate) fn with(override_json: serde_json::Value) -> Self {
        Repo::with_text(&serde_json::to_string_pretty(&override_json).unwrap())
    }

    pub(crate) fn with_text(text: &str) -> Self {
        let repo = Repo::empty();
        let file = project_pipeline_path(repo.path());
        fs::create_dir_all(file.parent().unwrap()).expect("create .claude-sessions");
        fs::write(&file, text).expect("write pipeline.json");
        repo
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    pub(crate) fn pipeline_file(&self) -> PathBuf {
        project_pipeline_path(self.path())
    }
}
