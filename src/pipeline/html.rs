//! The pipeline viewer's page: every flow, every step, and the exact prompt
//! text each contributes.
//!
//! Ported from the Node app's `src/pipeline/html.js`. Self-contained like the
//! journal page — inlined stylesheet, no script at all (the disclosures are
//! `<details>`), nothing fetched — so it opens from `file://` anywhere.
//!
//! It is generated from the same definitions that build the real prompt, which
//! is the point: this page cannot drift from what actually runs.

use std::path::Path;

use super::project::{project_pipeline_path, read_project_override};
use super::resolve::{resolve_pipeline, ResolvedPipeline, ResolvedStep};
use super::vars::{done_command, PromptVars};
use super::{StepKind, BUILT_IN};

const CSS: &str = include_str!("html/style.css");

/// A repo the viewer inspects for an override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepo {
    pub name: String,
    pub path: String,
}

/// Sample values so the rendered prompt reads like a real one.
///
/// Deliberately placeholder hosts and paths: this page is committed to nothing
/// and shown to anyone, so it must never carry a real Odoo URL.
pub fn sample_vars(task_id: i64, target_branch: &str) -> PromptVars {
    let summary_file = format!("~/.claude-sessions/summaries/task-{task_id}-summary.md");
    PromptVars {
        task_id,
        url: format!("https://odoo.example.com/web#id={task_id}&model=project.task&view_type=form"),
        extra_context: String::new(),
        archive_path: None,
        branch_instruction: format!("the `{target_branch}` branch"),
        target_branch: Some(target_branch.to_string()),
        repo_path: Some("/path/to/repo".to_string()),
        mr: None,
        resumed: false,
        extras: [(
            "doneCommand".to_string(),
            done_command(task_id, &summary_file),
        )]
        .into_iter()
        .collect(),
        summary_file,
    }
}

/// The default sample: a task id that is obviously an example, on the branch
/// most projects target.
pub fn default_sample_vars() -> PromptVars {
    sample_vars(5944, "development")
}

fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

fn step_card(
    pipeline: &ResolvedPipeline,
    step: &ResolvedStep,
    index: usize,
    vars: &PromptVars,
) -> String {
    let mut classes = vec!["step"];
    if step.skipped {
        classes.push("skipped");
    }
    if step.added {
        classes.push("added");
    } else if step.customised {
        classes.push("customised");
    }
    if step.gate {
        classes.push("gate");
    }
    if step.kind == StepKind::Dashboard {
        classes.push("dashboard");
    }

    let mut badges = String::new();
    if step.gate {
        badges.push_str(r#"<span class="badge gate">can stop here</span>"#);
    }
    // Dashboard steps are things the tool does; the agent is never told.
    if step.kind == StepKind::Dashboard {
        badges.push_str(r#"<span class="badge dash">the dashboard does this</span>"#);
    }
    if step.added {
        badges.push_str(r#"<span class="badge added">added by project</span>"#);
    } else if step.customised {
        badges.push_str(r#"<span class="badge custom">customised</span>"#);
    }
    if step.skipped {
        badges.push_str(r#"<span class="badge skip">skipped</span>"#);
    }

    let skill = step
        .skill
        .as_deref()
        .map(|skill| format!(r#"<div class="skill">/{}</div>"#, esc(skill)))
        .unwrap_or_default();
    let stage = match (step.kind, step.stage.as_deref()) {
        (StepKind::Dashboard, Some(stage)) => {
            format!(r#"<div class="skill">→ {}</div>"#, esc(stage))
        }
        _ => String::new(),
    };
    let detail = if step.detail.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="detail">{}</p>"#, esc(&step.detail))
    };
    let fragment = pipeline.prompt_for(step, vars);
    let prompt = if fragment.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<details><summary>prompt text</summary><pre>{}</pre></details>",
            esc(fragment.trim())
        )
    };

    format!(
        r#"
  <div class="{classes}">
    <div class="rail"><span class="dot"></span></div>
    <div class="card">
      <div class="card-head">
        <span class="idx">{index:02}</span>
        <h3>{title}</h3>
        {badges}
      </div>
      {skill}
      {stage}
      <p class="what">{what}</p>
      {detail}
      {prompt}
    </div>
  </div>"#,
        classes = classes.join(" "),
        title = esc(&step.title),
        what = esc(&step.what),
    )
}

fn pipeline_section(pipeline: &ResolvedPipeline, vars: &PromptVars, label: &str) -> String {
    let steps: String = pipeline
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| step_card(pipeline, step, index, vars))
        .collect();
    let active = pipeline.active_steps().len();
    let scope = if label.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="tag scope">{}</span>"#, esc(label))
    };
    let error = pipeline
        .override_error
        .as_deref()
        .map(|error| {
            format!(
                r#"<p class="error">Could not read {}: {}</p>"#,
                esc(&pipeline
                    .override_file
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_default()),
                esc(error)
            )
        })
        .unwrap_or_default();

    format!(
        r#"
  <section class="pipeline">
    <div class="pipeline-head">
      <h2>{name}</h2>
      <div class="meta">
        <span class="tag">{trigger}</span>
        <span class="tag">{active} step{plural}</span>
        {scope}
      </div>
    </div>
    <p class="summary">{summary}</p>
    {error}
    <div class="flow">{steps}</div>
    <details class="full"><summary>the complete prompt this sends</summary><pre>{prompt}</pre></details>
  </section>"#,
        name = esc(&pipeline.name),
        trigger = esc(&pipeline.trigger),
        plural = if active == 1 { "" } else { "s" },
        summary = esc(&pipeline.summary),
        prompt = esc(&pipeline.build_prompt(vars)),
    )
}

/// A project whose override actually changes something.
struct Customised {
    name: String,
    path: String,
    changed: usize,
    section: String,
}

fn customised_projects(projects: &[ProjectRepo], base: &PromptVars) -> Vec<Customised> {
    let mut out = Vec::new();
    for project in projects {
        let path = Path::new(&project.path);
        // No override, or one we could not parse — nothing to show for this repo.
        let Some(override_file) = read_project_override(Some(path)) else {
            continue;
        };
        if override_file.error.is_some() {
            continue;
        }
        let id = override_file.extends.as_deref().unwrap_or("task");
        let Ok(resolved) = resolve_pipeline(id, Some(path)) else {
            continue;
        };
        let changed = resolved
            .steps
            .iter()
            .filter(|step| step.customised || step.skipped)
            .count();
        if changed == 0 {
            continue;
        }
        let branch = override_file
            .vars
            .get("targetBranch")
            .cloned()
            .unwrap_or_else(|| "development".to_string());
        let vars = sample_vars(base.task_id, &branch);
        out.push(Customised {
            section: pipeline_section(&resolved, &vars, &project.name),
            name: project.name.clone(),
            path: project_pipeline_path(path).display().to_string(),
            changed,
        });
    }
    out
}

fn anchor(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '-' })
        .collect();
    format!("p-{slug}")
}

/// Render the whole page.
pub fn render_html(projects: &[ProjectRepo]) -> String {
    let vars = default_sample_vars();
    let defaults: String = BUILT_IN
        .iter()
        .filter_map(|pipeline| resolve_pipeline(pipeline.id, None).ok())
        .map(|resolved| pipeline_section(&resolved, &vars, "default"))
        .collect();

    let customised = customised_projects(projects, &vars);
    let project_nav = if customised.is_empty() {
        "<li class=\"empty\">No project overrides yet. Run <code>claude-sessions pipeline init &lt;repo&gt;</code>.</li>".to_string()
    } else {
        customised
            .iter()
            .map(|project| {
                format!(
                    r##"<li><a href="#{}">{}</a> <span class="n">{} changed</span></li>"##,
                    anchor(&project.name),
                    esc(&project.name),
                    project.changed
                )
            })
            .collect()
    };
    let project_sections: String = customised
        .iter()
        .map(|project| {
            format!(
                r#"
    <div id="{id}">
      <h2 class="project-name">{name}</h2>
      <p class="path">{path}</p>
      {section}
    </div>"#,
                id = anchor(&project.name),
                name = esc(&project.name),
                path = esc(&project.path),
                section = project.section,
            )
        })
        .collect();

    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>claude-sessions · task pipelines</title>
<style>{CSS}</style></head><body><div class="wrap">
<header>
  <h1>Task pipelines</h1>
  <p class="lede">What the dashboard actually tells an agent to do when you press <code>s</code> to start a task or <code>v</code> to handle a revision — every step, the skill it invokes, and the exact prompt text it contributes. Generated from the same definitions that build the prompt, so this cannot drift from what runs.</p>
  <nav>
    <h4>Projects with their own pipeline</h4>
    <ul>{project_nav}</ul>
  </nav>
</header>
{defaults}
{project_sections}
<footer>generated by claude-sessions pipeline · edit &lt;repo&gt;/.claude-sessions/pipeline.json to customise</footer>
</div></body></html>"#
    )
}

#[cfg(test)]
mod tests;
