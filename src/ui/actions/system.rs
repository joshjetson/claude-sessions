//! The runners that touch the machine: opening a URL, signalling a process,
//! closing a tab, playing a sound, opening an ssh session.
//!
//! Split out of the dispatcher so that file stays a map of "action -> who does
//! it". Every one of these passes [`SpawnPolicy::check`] before it starts
//! anything — that gate is the reason a test run cannot kill the developer's
//! own agents.

use std::process::{Command, ExitStatus, Stdio};
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
        // An empty pid list is not a failure: the agent is already gone and the
        // tab it left behind is exactly what a purge is for. Node closed it.
        if !entry.target.pids.is_empty() {
            if let Err(error) = kill_pids(&entry.target.pids, policy) {
                if let KillError::Failed(message) = &error {
                    crate::errorlog::report("kill", message);
                }
                failed += 1;
                continue;
            }
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
    // `afplay` is a macOS program, and the configured defaults are macOS system
    // sounds. Silence is already this function's failure mode, so starting a
    // process that cannot work buys nothing but a fork per notification.
    if cfg!(windows) {
        return;
    }
    if policy.check("play a sound").is_err() {
        return;
    }
    // No inherited stdio. `afplay` prints its complaint about a missing file
    // straight to the terminal, and on the alternate screen that text sits on
    // top of the dashboard until a restart.
    let child = Command::new("afplay")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(err) => {
            // The log only. A failed sound raised as a notification would ring
            // again, and fail again.
            crate::errorlog::record("sound", &format!("Could not play {file}: {err}"));
            return;
        }
    };
    // Waited on off the worker thread, so a sound never delays the next
    // action. The wait also reaps the child, which a dropped `Child` never
    // does: every notification used to leave a zombie behind until exit.
    let file = file.to_string();
    let _ = thread::Builder::new()
        .name("claude-sessions-sound".into())
        .spawn(move || {
            if let Ok(output) = child.wait_with_output() {
                if !output.status.success() {
                    crate::errorlog::record(
                        "sound",
                        &failure("afplay", &file, &output.status, &output.stderr),
                    );
                }
            }
        });
}

/// One sentence for a child that exited non-zero: what it was asked to do,
/// how it exited, and whatever it said on stderr.
fn failure(program: &str, target: &str, status: &ExitStatus, stderr: &[u8]) -> String {
    let said = String::from_utf8_lossy(stderr);
    let said = said.trim();
    if said.is_empty() {
        format!("{program} {target} exited with {status}")
    } else {
        format!("{program} {target} exited with {status}: {said}")
    }
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

/// Why a SIGTERM did not go out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillError {
    /// Nothing was attempted: no pids, no signals on this platform, or the
    /// spawn gate said no. A sentence for the status line, not an error.
    Refused(String),
    /// `kill` ran and failed, or could not start. A real error, for the log
    /// and the Notifications feed.
    Failed(String),
}

/// SIGTERM, through the same gate every other child process goes through.
///
/// `/bin/kill` rather than a raw syscall on purpose: [`SpawnPolicy`] guards
/// process *starts*, so routing the signal through one means a test run cannot
/// kill anything even by accident — which is exactly the failure the policy was
/// written for.
///
/// The child's stdout and stderr are captured, never inherited. `kill: 4242:
/// No such process` used to print straight onto the dashboard, and the exit
/// code was ignored, so the same keypress also flashed "Sent SIGTERM". A
/// non-zero exit is now a [`KillError::Failed`] that carries what `kill` said.
pub(super) fn kill_pids(pids: &[u32], policy: SpawnPolicy) -> Result<(), KillError> {
    if pids.is_empty() {
        return Err(KillError::Refused("Nothing to kill.".to_string()));
    }
    if !crate::platform::PROCESS_SIGNALS {
        return Err(KillError::Refused(crate::platform::unsupported(
            "Signalling a session",
        )));
    }
    policy
        .check("kill a session")
        .map_err(|refused| KillError::Refused(refused.message))?;
    let mut command = Command::new("kill");
    command.arg("-TERM");
    for pid in pids {
        command.arg(pid.to_string());
    }
    let listed = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(KillError::Failed(failure(
            "kill -TERM",
            &listed,
            &output.status,
            &output.stderr,
        ))),
        Err(err) => Err(KillError::Failed(format!(
            "Could not signal the session: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests;
