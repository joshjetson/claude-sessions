//! The runners that touch the machine: opening a URL, signalling a process,
//! closing a tab, playing a sound, opening an ssh session.
//!
//! Split out of the dispatcher so that file stays a map of "action -> who does
//! it". Every one of these passes [`SpawnPolicy::check`] before it starts
//! anything — that gate is the reason a test run cannot kill the developer's
//! own agents.

use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

use crate::ssh::{resolve_ssh_host, ssh_command, SshResolution, SSHING_COMMAND};
use crate::term::{Exec, LaunchRequest, SpawnPolicy, TerminalDriver};

use super::services::BoardServices;
use super::{ActionResult, PURGE_GRACE, PURGE_REFRESH_DELAYS};

/// `open <url>` — the one shell-out that is not a terminal driver call.
pub(super) fn open_with(target: &str, policy: SpawnPolicy, results: &Sender<ActionResult>) {
    let result = Exec::new(policy).open(target);
    if !result.ok {
        let _ = results.send(ActionResult::Flash(format!(
            "Could not open {target}: {}",
            result.failure_message()
        )));
    }
}

/// Close out a set of finished sessions: signal the agent, then close its tab.
///
/// Kill first, close second. Closing the tab under a live agent leaves it
/// running headless with nowhere to report, which is worse than either. The
/// 150ms between them is the grace the agent needs to actually go.
pub(super) fn purge(
    entries: &[crate::purge::PurgeEntry],
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    if let Err(refused) = policy.check(&format!("purge {} session(s)", entries.len())) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    let mut closed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        if kill_pids(&entry.target.pids, policy).is_err() {
            failed += 1;
            continue;
        }
        thread::sleep(PURGE_GRACE);
        let reference = crate::term::SessionRef {
            tty: entry.target.tty.clone(),
            session_id: Some(entry.target.session_id.clone()),
            cwd: None,
        };
        // No terminal to close: the agent is gone, which is the whole job.
        if reference.tty_device().is_none() {
            closed += 1;
            continue;
        }
        if driver.close(&reference).ok {
            closed += 1;
        } else {
            failed += 1;
        }
    }
    let _ = results.send(ActionResult::Flash(if failed > 0 {
        format!(
            "Purged {closed} of {} — {failed} tab(s) would not close.",
            entries.len()
        )
    } else {
        format!("Purged {closed} session(s). Transcripts are archived; v reopens any of them.")
    }));
    // SIGTERM is not instant and a closed tab takes a beat to leave `ps`.
    for delay in PURGE_REFRESH_DELAYS {
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        if results.send(ActionResult::Refresh).is_err() {
            return;
        }
    }
}

/// A notification sound. Failure is silence, which is the correct failure mode
/// for a sound.
pub(super) fn play(file: &str, policy: SpawnPolicy) {
    if policy.check("play a sound").is_err() {
        return;
    }
    let _ = Command::new("afplay").arg(file).spawn();
}

/// Open a terminal on the server a project runs on.
///
/// `~/.ssh/config` is the source of truth — the same file `sshing` reads — so a
/// host resolved here is the host you would pick there. An ambiguous match is
/// never guessed between: connecting to the wrong server is worse than not
/// connecting.
pub(super) fn ssh(
    project: &str,
    services: &BoardServices,
    driver: &Arc<dyn TerminalDriver>,
    policy: SpawnPolicy,
    results: &Sender<ActionResult>,
) {
    if let Err(refused) = policy.check(&format!("open an ssh session for {project}")) {
        let _ = results.send(ActionResult::Flash(refused.message));
        return;
    }
    let config =
        crate::config::ConfigHandle::load(&services.paths, crate::config::EnvOverrides::from_env());
    let hosts = crate::ssh::load_ssh_hosts(&crate::ssh::ssh_config_path(&services.paths.home));
    let (command, say) = match resolve_ssh_host(project, &hosts, config.ssh_hosts()) {
        SshResolution::Host(host) => {
            let mut say = String::new();
            // A malformed HostName produces "could not resolve hostname", which
            // says nothing about the real cause. Say it before connecting.
            if let Some(problem) = &host.problem {
                say = format!("~/.ssh/config: {} Fix: {}", problem.message, problem.fix);
            }
            (ssh_command(&host.alias), say)
        }
        SshResolution::Ambiguous(aliases) => (
            SSHING_COMMAND.to_string(),
            format!(
                "{project} matches several hosts ({}) — opening sshing to choose.",
                aliases.join(", ")
            ),
        ),
        SshResolution::NoMatch => (
            SSHING_COMMAND.to_string(),
            format!("No ssh host matches {project} — opening sshing."),
        ),
    };
    if !say.is_empty() {
        let _ = results.send(ActionResult::Flash(say));
    }
    let home = services.paths.home.to_string_lossy().into_owned();
    let request = LaunchRequest::new(home, command.clone()).title(format!("ssh {project}"));
    let result = driver.launch(&request);
    if !result.ok {
        let reason = result.error.unwrap_or_else(|| "unknown reason".into());
        let _ = results.send(ActionResult::Flash(format!(
            "Could not open a terminal for {project}: {reason}"
        )));
    }
}

/// SIGTERM, through the same gate every other child process goes through.
///
/// `/bin/kill` rather than a raw syscall on purpose: [`SpawnPolicy`] guards
/// process *starts*, so routing the signal through one means a test run cannot
/// kill anything even by accident — which is exactly the failure the policy was
/// written for.
pub(super) fn kill_pids(pids: &[u32], policy: SpawnPolicy) -> Result<(), String> {
    if pids.is_empty() {
        return Err("Nothing to kill.".to_string());
    }
    policy
        .check("kill a session")
        .map_err(|refused| refused.message)?;
    let mut command = Command::new("kill");
    command.arg("-TERM");
    for pid in pids {
        command.arg(pid.to_string());
    }
    match command.status() {
        Ok(_) => Ok(()),
        Err(err) => Err(format!("Could not signal the session: {err}")),
    }
}
