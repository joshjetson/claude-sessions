//! `claude-sessions pipeline` — the pipeline viewer, and the three terminal
//! subcommands beside it.
//!
//! Ported from the Node app's `bin/pipeline-viewer.js`:
//! `init <repo>`, `skills [filter]`, `show [id]`, and the default HTML build.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::ConfigHandle;
use crate::paths::Paths;
use crate::pipeline::html::{render_html, ProjectRepo};
use crate::pipeline::{
    discover_skills, init_project_pipeline, pipeline_skills, resolve_pipeline, StepKind, BUILT_IN,
};
use crate::term::{Exec, SpawnPolicy};

/// Where the page is written unless `--out` says otherwise.
pub fn default_out(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join("pipeline.html")
}

pub fn run(
    paths: &Paths,
    config: &ConfigHandle,
    args: super::PipelineArgs,
    policy: SpawnPolicy,
) -> Result<()> {
    match args.action {
        Some(super::PipelineAction::Init { repo, pipeline }) => {
            init(paths, &repo, pipeline.as_deref().unwrap_or("task"))
        }
        Some(super::PipelineAction::Skills { filter, repo }) => {
            skills(paths, filter.as_deref(), repo.as_deref())
        }
        Some(super::PipelineAction::Show { id, repo }) => {
            show(paths, id.as_deref().unwrap_or("task"), repo.as_deref())
        }
        None => build(paths, config, args.out, args.no_open, policy),
    }
}

fn init(paths: &Paths, repo: &Path, pipeline_id: &str) -> Result<()> {
    match init_project_pipeline(&expand(paths, repo), pipeline_id) {
        Ok(file) => {
            println!("wrote {}", file.display());
            println!("Edit it, then re-run `claude-sessions pipeline` to see the change.");
            Ok(())
        }
        Err(error) => bail!("init: {error}"),
    }
}

fn skills(paths: &Paths, filter: Option<&str>, repo: Option<&Path>) -> Result<()> {
    let repo = repo.map(|repo| expand(paths, repo));
    let all = discover_skills(&paths.claude_dir, repo.as_deref());
    let needle = filter.map(str::to_lowercase);
    let shown: Vec<_> = all
        .iter()
        .filter(|skill| match &needle {
            Some(needle) => {
                skill.name.to_lowercase().contains(needle)
                    || skill.description.to_lowercase().contains(needle)
            }
            None => true,
        })
        .collect();

    let used = pipeline_skills();
    let matching = filter
        .map(|filter| format!(" matching \"{filter}\""))
        .unwrap_or_default();
    println!(
        "\n{} skill{}{matching} — put one in a step's \"skill\" field\n",
        shown.len(),
        if shown.len() == 1 { "" } else { "s" },
    );
    for skill in shown {
        let mark = if used.contains(&skill.name.as_str()) {
            '*'
        } else {
            ' '
        };
        let description: String = skill
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let (description, ellipsis) = if description.chars().count() > 96 {
            (description.chars().take(96).collect::<String>(), "…")
        } else {
            (description, "")
        };
        println!("{mark} {:<30} {description}{ellipsis}", skill.name);
    }
    println!("\n* already used by the built-in pipelines\n");
    Ok(())
}

fn show(paths: &Paths, id: &str, repo: Option<&Path>) -> Result<()> {
    let repo = repo.map(|repo| expand(paths, repo));
    let Ok(resolved) = resolve_pipeline(id, repo.as_deref()) else {
        let known: Vec<&str> = BUILT_IN.iter().map(|pipeline| pipeline.id).collect();
        bail!(
            "show: unknown pipeline \"{id}\" (have: {})",
            known.join(", ")
        );
    };

    println!("\n{}  —  {}", resolved.name, resolved.trigger);
    if let Some(file) = &resolved.override_file {
        println!("overrides: {}", file.display());
    }
    println!();
    for (index, step) in resolved.steps.iter().enumerate() {
        let mut marks: Vec<&str> = Vec::new();
        if step.kind == StepKind::Dashboard {
            marks.push("dashboard");
        }
        if step.gate {
            marks.push("gate");
        }
        if step.added {
            marks.push("added");
        } else if step.customised {
            marks.push("custom");
        }
        if step.skipped {
            marks.push("skipped");
        }
        let suffix = if marks.is_empty() {
            String::new()
        } else {
            format!("  [{}]", marks.join(" · "))
        };
        println!("  {index:02}  {}{suffix}", step.title);
        if let Some(skill) = &step.skill {
            println!("      /{skill}");
        }
        if step.kind == StepKind::Dashboard {
            if let Some(stage) = &step.stage {
                println!("      → {stage}");
            }
        }
        println!("      {}", step.what);
    }
    println!(
        "\n{} of {} steps run.\n",
        resolved.active_steps().len(),
        resolved.steps.len()
    );
    Ok(())
}

fn build(
    paths: &Paths,
    config: &ConfigHandle,
    out: Option<PathBuf>,
    no_open: bool,
    policy: SpawnPolicy,
) -> Result<()> {
    let projects = known_projects(paths, config);
    let out = out.unwrap_or_else(|| default_out(paths));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(&out, render_html(&projects))
        .with_context(|| format!("could not write {}", out.display()))?;

    let with_override = projects
        .iter()
        .filter(|project| {
            resolve_pipeline("task", Some(Path::new(&project.path)))
                .map(|resolved| resolved.source == crate::pipeline::PipelineSource::Project)
                .unwrap_or(false)
        })
        .count();
    println!(
        "{} pipelines · {} project repos · {with_override} with their own pipeline",
        BUILT_IN.len(),
        projects.len(),
    );
    println!("{}", out.display());
    if !no_open {
        let result = Exec::new(policy).open(&out.display().to_string());
        if !result.ok {
            eprintln!("could not open it: {}", result.failure_message());
        }
    }
    Ok(())
}

/// Every repo the board can spawn work into is a candidate for an override.
fn known_projects(paths: &Paths, config: &ConfigHandle) -> Vec<ProjectRepo> {
    let mut out = Vec::new();
    for name in config.odoo_project_names() {
        for dir in config.odoo_project_dir_list(name) {
            out.push(ProjectRepo {
                name: name.to_string(),
                path: expand(paths, Path::new(dir)).display().to_string(),
            });
        }
    }
    out
}

fn expand(paths: &Paths, path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => paths.home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}
