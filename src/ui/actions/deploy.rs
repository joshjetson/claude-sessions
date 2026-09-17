//! The Deploy tab's side of the worker thread: loading the board, merging, and
//! asking the daemon to run a deploy.
//!
//! Ported from `mergeTaskMR` / `mergeAllReady` / `startDeploy` / `cancelDeploy`
//! / `refreshDeployBoard` in the Node app's `src/tui/deployactions.js`. Every
//! one of these blocks on a network round trip, which is why none of them can
//! live in a key handler (brief §10 mandate #9).

use std::sync::mpsc::Sender;

use crate::config::{ConfigHandle, EnvOverrides};
use crate::deploy::{deploy_specs, enrich_with_live_mrs, fetch_deploy_board};
use crate::gitlab::MergeOptions;
use crate::ui::deploy::DeployUpdate;
use crate::ui::state::MergeTarget;
use crate::util::truncate;

use super::{ActionResult, BoardData, BoardServices};

/// Reload the deploy board: Odoo for the Deployed-stage tasks, then one `glab`
/// read per merge request so the badges are live rather than Odoo's cache.
pub fn refresh(services: &BoardServices, results: &Sender<ActionResult>) {
    let Some(odoo) = &services.odoo else {
        return send(
            results,
            DeployUpdate::failed(
                "No Odoo credentials — set the odoo block in ~/.claude-sessions.json.",
            ),
        );
    };
    // Re-read rather than held: the deploy config is edited from a dialog on
    // this very tab, and a worker holding its own copy would deploy the old
    // command.
    let config = ConfigHandle::load(&services.paths, EnvOverrides::from_env());
    let specs = deploy_specs(&config);
    match fetch_deploy_board(odoo, &specs) {
        Ok(mut board) => {
            enrich_with_live_mrs(&services.gitlab, &mut board);
            send(results, DeployUpdate::loaded(board));
        }
        Err(error) => send(results, DeployUpdate::failed(error)),
    }
}

/// Merge each target in turn.
///
/// Sequentially, never in parallel: a failure halfway has to name the merge
/// request it failed on, and GitLab's own reason for it, or the user is left
/// guessing which of eight merges did not happen.
pub fn merge(
    services: &BoardServices,
    project: &str,
    targets: &[MergeTarget],
    results: &Sender<ActionResult>,
) {
    if targets.is_empty() {
        return flash(results, format!("No MRs ready to merge in {project}."));
    }
    let mut merged = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        flash(
            results,
            format!(
                "Merging !{} (#{})… [{}/{}]",
                target.iid,
                target.task_id,
                index + 1,
                targets.len()
            ),
        );
        match services
            .gitlab
            .merge_mr(&target.project_path, target.iid, MergeOptions::default())
        {
            Ok(_) => merged += 1,
            // GitLab's sentence, unedited: it names the actual blocker.
            Err(error) => failures.push(format!("!{}: {error}", target.iid)),
        }
    }

    if failures.is_empty() {
        let plural = if merged == 1 { "" } else { "s" };
        let what = match targets {
            [only] => format!(" — #{} {}", only.task_id, truncate(&only.name, 40)),
            _ => String::new(),
        };
        flash(
            results,
            format!("✓ Merged {merged} MR{plural} in {project}{what}."),
        );
    } else {
        flash(
            results,
            format!(
                "Merged {merged}/{}. Failed:\n{}",
                targets.len(),
                failures.join("\n")
            ),
        );
    }
    // The rows carry the MR state, so the board is refetched either way.
    refresh(services, results);
}

/// The current user's open merge requests, for the board tab's `M`.
pub fn open_mrs(services: &BoardServices, results: &Sender<ActionResult>) {
    let data = match services
        .gitlab
        .fetch_open_mrs(crate::gitlab::MrScope::CreatedByMe)
    {
        Ok(mrs) => BoardData::OpenMrs(mrs),
        Err(error) => BoardData::Failed {
            task_id: None,
            error: error.to_string(),
        },
    };
    let _ = results.send(ActionResult::Data(Box::new(data)));
}

fn send(results: &Sender<ActionResult>, update: DeployUpdate) {
    let _ = results.send(ActionResult::Deploy(Box::new(update)));
}

fn flash(results: &Sender<ActionResult>, message: String) {
    let _ = results.send(ActionResult::Flash(message));
}
