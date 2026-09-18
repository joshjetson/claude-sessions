//! The board half of the worker: the launch, the lookups, and the stage moves.
//!
//! Everything here runs OFF the draw thread. The specs it is handed are already
//! resolved — see [`crate::ui::board::spec`] — so this file decides nothing; it
//! writes the prompt file, opens the terminal, and reports what happened.
//!
//! Every process start goes through [`SpawnPolicy::check`]. A test that walked
//! past the start gate in the Node app reached the real launch path, found the
//! user's live project-to-directory mapping, and opened terminal tabs running
//! `claude --dangerously-skip-permissions` against production task data —
//! sixteen prompt files and several running agents came out of test runs before
//! anyone noticed.

use std::fs;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::archive::Archive;
use crate::daemon::{StageMove, StageMoveRequest};
use crate::db::Db;
use crate::odoo::FetchBoardOptions;
use crate::paths::Paths;
use crate::term::{LaunchRequest, SpawnPolicy, TerminalDriver};
use crate::transcript::TaskRefCache;
use crate::ui::board::{BoardUpdate, LaunchSpec, NudgeSpec, ResumeRequest, SendSpec};

use super::services::BoardServices;
use super::{ActionResult, BoardData};

/// Fetch the board, mapping whatever happens onto one update.
pub fn refresh_board(
    services: &BoardServices,
    options: &FetchBoardOptions,
    results: &Sender<ActionResult>,
) {
    let filter = if options.mine_only {
        crate::daemon::BoardFilter::Mine
    } else {
        crate::daemon::BoardFilter::All
    };
    let update = match &services.odoo {
        None => BoardUpdate::failed(
            filter,
            "No Odoo credentials — set the odoo block in ~/.claude-sessions.json.",
        ),
        Some(client) => match client.fetch_board(options) {
            Ok(board) => {
                let mut update = BoardUpdate::loaded(filter, board);
                update.optics_tasks = optics_coverage(services, update.board.as_ref());
                update
            }
            // Best-effort: the previous board stays on screen with the reason
            // beside it rather than the tab going blank.
            Err(error) => BoardUpdate::failed(filter, error.to_string()),
        },
    };
    let _ = results.send(ActionResult::Board(Box::new(update)));
}

/// Recorded Optics coverage for everything on the board: one query per
/// project, merged into one map.
///
/// Rides the board fetch rather than polling on its own — the answer is only
/// useful next to the rows it annotates, and it is already off the draw thread
/// here. `None` for the client is the normal case (Optics is opt-in and needs
/// both an endpoint and a token), and every error inside resolves to no
/// coverage: a missing badge is a nuisance, a board that will not render is a
/// fault.
fn optics_coverage(
    services: &BoardServices,
    board: Option<&crate::types::Board>,
) -> std::collections::HashMap<i64, usize> {
    let mut coverage = std::collections::HashMap::new();
    let (Some(optics), Some(board)) = (&services.optics, board) else {
        return coverage;
    };
    for (project_name, project) in &board.projects {
        let task_ids: Vec<i64> = project
            .stages
            .values()
            .flat_map(|stage| stage.tasks.iter())
            .flat_map(|task| std::iter::once(task.id).chain(task.subtasks.iter().map(|sub| sub.id)))
            .collect();
        coverage.extend(optics.project_coverage(project_name, &task_ids));
    }
    coverage
}

/// One Odoo lookup a dialog is waiting on.
pub fn fetch(services: &BoardServices, data: Lookup, results: &Sender<ActionResult>) {
    let Some(client) = &services.odoo else {
        let _ = results.send(ActionResult::Data(Box::new(BoardData::Failed {
            task_id: data.task_id(),
            error: "No Odoo credentials configured.".to_string(),
        })));
        return;
    };
    let task_id = data.task_id();
    let answer = match &data {
        Lookup::Blockers {
            task_id,
            blocker_ids,
        } => client
            .fetch_blockers(blocker_ids)
            .map(|blockers| BoardData::Blockers {
                task_id: *task_id,
                blockers,
            }),
        Lookup::Stages {
            task_id,
            project_id,
        } => client
            .get_project_stages(*project_id)
            .map(|stages| BoardData::Stages {
                task_id: *task_id,
                stages,
            }),
        Lookup::Projects => client.get_projects().map(BoardData::Projects),
        Lookup::TaskStages { task_ids } => client
            .fetch_stages_for_tasks(task_ids)
            .map(|stages| BoardData::TaskStages(stages.into_iter().collect())),
        Lookup::TaskDescription { task_id } => {
            client
                .get_task_detail(*task_id)
                .map(|detail| BoardData::TaskDescription {
                    task_id: *task_id,
                    detail,
                })
        }
    };
    let payload = match answer {
        Ok(data) => data,
        Err(error) => BoardData::Failed {
            task_id,
            error: error.to_string(),
        },
    };
    let _ = results.send(ActionResult::Data(Box::new(payload)));
}

/// The lookups, as one type so the worker has one arm rather than four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Blockers {
        task_id: i64,
        blocker_ids: Vec<i64>,
    },
    Stages {
        task_id: i64,
        project_id: i64,
    },
    Projects,
    TaskDescription {
        task_id: i64,
    },
    /// Which stage each of these tasks is in — the purge dialog's question,
    /// asked once for every session the board could not account for.
    TaskStages {
        task_ids: Vec<i64>,
    },
}

impl Lookup {
    fn task_id(&self) -> Option<i64> {
        match self {
            Lookup::Blockers { task_id, .. }
            | Lookup::Stages { task_id, .. }
            | Lookup::TaskDescription { task_id } => Some(*task_id),
            Lookup::Projects | Lookup::TaskStages { .. } => None,
        }
    }
}

/// Open a terminal on a resolved launch.
pub fn launch(
    spec: &LaunchSpec,
    services: &BoardServices,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    if let Err(refused) = policy.check(&format!("launch a session for task {}", spec.task_id)) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    // The agent is told to write its write-up here; a missing directory turns a
    // finished task into a silent one.
    let _ = fs::create_dir_all(&services.paths.summaries_dir);

    let command = match &spec.prompt {
        None => format!("claude {}", spec.flags).trim_end().to_string(),
        Some(prompt) => match write_prompt(&services.paths, spec.task_id, prompt) {
            Err(error) => {
                let _ = results.send(ActionResult::Flash(format!(
                    "Could not write the prompt for #{}: {error}",
                    spec.task_id
                )));
                return;
            }
            // Through a file: the prompt is far too long for a command line,
            // and embedding it would mean escaping it for both the shell and
            // the terminal driver.
            Ok(file) => format!(
                "claude {} \"$(cat '{}')\"",
                spec.flags,
                file.replace('\'', "'\\''")
            )
            .replace("  ", " "),
        },
    };

    let request = LaunchRequest::new(spec.cwd.clone(), command)
        .task_id(spec.task_id)
        .title(spec.title.clone());
    let result = driver.launch(&request);
    if !result.ok {
        let reason = result.error.unwrap_or_else(|| "unknown reason".into());
        let _ = results.send(ActionResult::Flash(format!(
            "Couldn't open a terminal for task {}: {reason}",
            spec.task_id
        )));
        return;
    }
    let _ = results.send(ActionResult::Launched);
    if !spec.say.is_empty() {
        let _ = results.send(ActionResult::Flash(spec.say.clone()));
    }
    move_stage(spec.stage_move.as_ref(), services, results);
}

/// Type a revision into a session that is already open, then go there so you
/// can watch it land and answer anything it asks.
pub fn send_to_session(
    spec: &SendSpec,
    services: &BoardServices,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    let short: String = spec.session_id.chars().take(8).collect();
    if let Err(refused) = policy.check(&format!("type into session {short}")) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    let sent = driver.send_text(&spec.session, &spec.prompt);
    if !sent.ok {
        let reason = sent.error.unwrap_or_else(|| "unknown reason".into());
        let _ = results.send(ActionResult::Flash(format!(
            "Could not send the revision to session {short}: {reason}"
        )));
        return;
    }
    let _ = driver.focus(&spec.session);
    let _ = results.send(ActionResult::Flash(format!(
        "Revision sent to session {short}."
    )));
    move_stage(spec.stage_move.as_ref(), services, results);
}

/// Wake a run's coordinator so it reads a question that was just asked.
///
/// Sends and stops. It does NOT focus the terminal, and it does not flash on
/// success: a run asks many questions, and a toast for each one would bury
/// everything else the reviewer needs to see. A failure still speaks up,
/// because a coordinator that is not being woken is a run that has quietly
/// stopped answering anything.
pub fn nudge_coordinator(
    spec: &NudgeSpec,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    let short: String = spec.session_id.chars().take(8).collect();
    if let Err(refused) = policy.check(&format!("type into session {short}")) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    let sent = driver.send_text(&spec.session, &spec.text);
    if !sent.ok {
        let reason = sent.error.unwrap_or_else(|| "unknown reason".into());
        let _ = results.send(ActionResult::Flash(format!(
            "Could not reach the coordinator for {}: {reason}",
            spec.run_id
        )));
    }
}

/// Find the archived conversation for a task, then launch against it.
///
/// `ensure_live_session` also restores the transcript into Claude Code's own
/// project directory when it has been cleaned up, which is what `--resume`
/// reads — so this is genuinely file I/O and belongs off the draw thread.
pub fn resume(
    request: &ResumeRequest,
    services: &BoardServices,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    let task_id = request.prompt.task_id;
    let db = Db::open(&services.paths);
    let archive = Archive::new(&services.paths, &db);
    let mut refs = TaskRefCache::default();
    let meta = archive.ensure_live_session(task_id, &mut refs);
    // Conflict resolution is the one purpose that proceeds with nothing
    // archived: the merge request still has to be reconciled.
    if meta.is_none() && !request.purpose.starts_fresh() {
        let _ = results.send(ActionResult::Flash(match request.purpose {
            crate::ui::board::ResumePurpose::Revision => {
                format!("No archived transcript for #{task_id} yet — nothing to resume.")
            }
            _ => format!("No conversation archived for #{task_id} yet — nothing to resume."),
        }));
        return;
    }
    let session_id = meta
        .as_ref()
        .map(|meta| meta.session_id.clone())
        .unwrap_or_default();
    let cwd = match meta.as_ref().map(|meta| meta.cwd.clone()) {
        Some(cwd) if !cwd.is_empty() => cwd,
        _ => request.link_cwd.clone(),
    };
    if cwd.is_empty() {
        let _ = results.send(ActionResult::Flash(
            "No working directory recorded for that session.".to_string(),
        ));
        return;
    }
    let archive_path = archive
        .archive_path(task_id)
        .map(|path| path.to_string_lossy().into_owned());
    match request.spec(&session_id, &cwd, archive_path) {
        Ok(spec) => launch(&spec, services, driver, policy, results),
        Err(error) => {
            let _ = results.send(ActionResult::Flash(error.to_string()));
        }
    }
}

/// The best-effort work-ward move that rides along with a launch.
///
/// Fire and forget by design: an Odoo hiccup must never stop you starting work.
/// It is not silent, though — a failed move used to leave a task sitting in
/// "Approved to Start" with nothing to say so.
fn move_stage(
    request: Option<&StageMoveRequest>,
    services: &BoardServices,
    results: &Sender<ActionResult>,
) {
    let (Some(request), Some(backend)) = (request, &services.backend) else {
        return;
    };
    let message = match backend.move_to_stage(request) {
        Ok(StageMove::Moved(stage)) => format!("#{} → {stage}.", request.task_id),
        // Nothing to say: the project turned the move off, or it is already
        // there.
        Ok(StageMove::Disabled) | Ok(StageMove::Unchanged(_)) => return,
        Ok(StageMove::NoStage(reason)) => format!("#{} left in place — {reason}.", request.task_id),
        Err(error) => format!("Could not move #{}: {error}", request.task_id),
    };
    let _ = results.send(ActionResult::Flash(message));
}

/// The explicit move from the stage picker: the stage was named, so nothing is
/// resolved and nothing is guessed.
pub fn move_to_named_stage(
    services: &BoardServices,
    task_id: i64,
    stage_id: i64,
    stage_name: &str,
    results: &Sender<ActionResult>,
) {
    let Some(client) = &services.odoo else {
        let _ = results.send(ActionResult::Flash(
            "No Odoo credentials configured.".to_string(),
        ));
        return;
    };
    match client.move_stage(task_id, stage_id) {
        Ok(()) => {
            let _ = results.send(ActionResult::Flash(format!("#{task_id} → {stage_name}.")));
        }
        Err(error) => {
            let _ = results.send(ActionResult::Flash(format!("Move failed: {error}")));
        }
    }
}

/// Write the prompt to a file and return its path.
fn write_prompt(paths: &Paths, task_id: i64, prompt: &str) -> std::io::Result<String> {
    fs::create_dir_all(&paths.prompts_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    let file = paths
        .prompts_dir
        .join(format!("task-{task_id}-{stamp}.txt"));
    fs::write(&file, prompt)?;
    Ok(file.to_string_lossy().into_owned())
}

/// The starter `pipeline.json`, from the viewer's `t`.
pub fn write_pipeline_template(repo: &str, pipeline_id: &str, results: &Sender<ActionResult>) {
    let message = match crate::pipeline::init_project_pipeline(Path::new(repo), pipeline_id) {
        Ok(file) => format!("Wrote {} — press e to edit it.", file.display()),
        Err(error) => error.to_string(),
    };
    let _ = results.send(ActionResult::Flash(message));
}
