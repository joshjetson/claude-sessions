//! The starter `pipeline.json` a project gets, and finding a step inside it.
//!
//! The file is written line by line rather than by serialising one object, so
//! each step keeps its own commented hints: opening the file should tell you
//! what you can do at that step without looking it up. Every literal still goes
//! through the JSON serialiser, so quoting inside the help text cannot corrupt
//! the file.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::definitions::{built_in, PipelineDef, StepKind};
use super::project::{project_pipeline_path, PROJECT_DIR};
// Help text a person reads and types, so the bare name is right here — an
// absolute path belongs only in a PROMPT, where PATH cannot be trusted.
use super::vars::BIN_NAME as BIN;

#[derive(Debug)]
pub enum InitError {
    /// Never clobber: a project's real overrides are not recoverable from here.
    Exists(PathBuf),
    Unknown(String),
    Io(io::Error),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::Exists(file) => write!(f, "{} already exists", file.display()),
            InitError::Unknown(id) => write!(f, "Unknown pipeline \"{id}\""),
            InitError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for InitError {}

/// Write the starter override into a repo. Never clobbers an existing one.
pub fn init_project_pipeline(repo_path: &Path, pipeline_id: &str) -> Result<PathBuf, InitError> {
    let file = project_pipeline_path(repo_path);
    if file.exists() {
        return Err(InitError::Exists(file));
    }
    let base = built_in(pipeline_id).ok_or_else(|| InitError::Unknown(pipeline_id.to_string()))?;

    fs::create_dir_all(repo_path.join(PROJECT_DIR)).map_err(InitError::Io)?;
    fs::write(&file, starter_template(base)).map_err(InitError::Io)?;
    Ok(file)
}

/// The starter file: valid JSON, and inert until somebody removes a `$`.
pub fn starter_template(base: &PipelineDef) -> String {
    let doc: Vec<String> = vec![
        format!("Pipeline overrides for this repository — {}.", base.name),
        String::new(),
        "HOW THIS FILE WORKS".to_string(),
        "  Keys starting with $ are NOTES and are ignored. To change something, add".to_string(),
        "  the same key WITHOUT the $:".to_string(),
        "      \"append\": \"...\"      <- takes effect".to_string(),
        "      \"$edit\":  \"...\"      <- just a hint, does nothing".to_string(),
        String::new(),
        "THE ONE TO GET RIGHT".to_string(),
        "  \"append\" keeps what a step already says and adds yours after it.".to_string(),
        "  \"prompt\" REPLACES the step entirely.".to_string(),
        String::new(),
        "  This matters most on \"finish\". Its default instruction is what tells the".to_string(),
        format!("  agent to run `{BIN} done`, which is what makes the dashboard post the"),
        "  summary to Odoo, move the task to QA, write the standup line and ARCHIVE".to_string(),
        "  the conversation. Replace it and all four stop happening, silently."
            .to_string(),
        String::new(),
        "      \"finish\": { \"append\": \"Merge to development when done.\" }   <- yes".to_string(),
        format!(
            "      \"finish\": {{ \"prompt\": \"Merge to development when done.\" }}   <- loses `{BIN} done`"
        ),
        String::new(),
        "EVERY KEY YOU CAN SET ON A STEP".to_string(),
        "  append   add a sentence after what the step already says".to_string(),
        "  prompt   replace the step's instruction entirely".to_string(),
        "  run      a command to run at this step, e.g. \"./scripts/deploy.sh {{taskId}}\"".to_string(),
        "  skip     true — drop it from the run (still shown, marked skipped)".to_string(),
        format!("  skill    the skill it invokes; list them: {BIN} pipeline skills"),
        "  stage    stage-move steps only: which Odoo stage to move into".to_string(),
        "  title    relabel it in the diagram".to_string(),
        "  after / before   where a NEW step sits (see below)".to_string(),
        String::new(),
        "ADDING A STEP".to_string(),
        "  A new step with no after/before goes LAST — after \"finish\". Position it:".to_string(),
        "      \"smoke\": { \"before\": \"finish\", \"title\": \"Smoke test\",".to_string(),
        "                 \"prompt\": \"Run ./scripts/smoke.sh and paste the result in the MR.\" }".to_string(),
        String::new(),
        "VARIABLES  (inside prompt / append / run)".to_string(),
        "  {{taskId}}  {{url}}  {{targetBranch}}  {{summaryFile}}".to_string(),
        String::new(),
        "CHECK YOUR WORK".to_string(),
        format!("  {BIN} pipeline show task --repo .    the resolved flow"),
        format!("  {BIN} pipeline                       the same as a page, with prompt text"),
        "  ...or press P in the dashboard. After editing \"finish\", confirm the flow".to_string(),
        "  still ends with the completion instruction.".to_string(),
    ];

    let mut out: Vec<String> = Vec::new();
    out.push("{".to_string());
    out.push("  \"$doc\": [".to_string());
    let last_doc = doc.len() - 1;
    for (index, line) in doc.iter().enumerate() {
        let comma = if index == last_doc { "" } else { "," };
        out.push(format!("    {}{comma}", json_string(line)));
    }
    out.push("  ],".to_string());
    out.push(String::new());
    out.push(format!("  \"extends\": {},", json_string(base.id)));
    out.push(String::new());
    out.push("  \"vars\": {".to_string());
    out.push(format!(
        "    \"$example\": {}",
        json_string("targetBranch: \"main\" overrides the branch MRs target")
    ));
    out.push("  },".to_string());
    out.push(String::new());
    out.push("  \"steps\": {".to_string());

    let last_step = base.steps.len() - 1;
    for (index, step) in base.steps.iter().enumerate() {
        // The hint is per-kind: `prompt` is meaningless on a step the agent
        // never sees, and `stage` is meaningless on one the dashboard never
        // acts on.
        let edit = match step.kind {
            StepKind::Dashboard => {
                "add \"stage\": \"Testing\" to move it elsewhere, or \"skip\": true to not move it at all"
            }
            StepKind::Agent => {
                "add \"skip\": true, \"prompt\"/\"append\": \"...\", \"run\": \"./cmd.sh\", \"skill\", or \"title\""
            }
        };
        out.push(format!("    {}: {{", json_string(step.id)));
        out.push(format!("      \"$what\": {},", json_string(step.what)));
        if let Some(skill) = step.skill {
            out.push(format!("      \"$skill\": {},", json_string(skill)));
        }
        out.push(format!("      \"$edit\": {}", json_string(edit)));
        out.push(format!(
            "    }}{}",
            if index == last_step { "" } else { "," }
        ));
    }

    out.push("  }".to_string());
    out.push("}".to_string());
    out.join("\n") + "\n"
}

/// The 1-based line where a step's key appears, so an editor can open directly
/// on the step you were looking at. Anything unreadable answers line 1 — the
/// editor still opens, just at the top.
pub fn step_line_number(file: &Path, step_id: &str) -> usize {
    let Ok(text) = fs::read_to_string(file) else {
        return 1;
    };
    let needle = format!("\"{step_id}\"");
    text.lines()
        .position(|line| line.contains(&needle))
        .map(|index| index + 1)
        .unwrap_or(1)
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}
