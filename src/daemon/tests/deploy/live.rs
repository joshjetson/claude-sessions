//! The one test in this crate that spawns a deploy.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::daemon::tests::{engine_with, Setup};
use crate::term::SpawnPolicy;
use crate::types::DeployRunStatus;

// --- the one test that runs a real process ------------------------------------

/// THE ONLY TEST IN THIS CRATE THAT SPAWNS A DEPLOY.
///
/// Everything else runs under [`SpawnPolicy::Refuse`]. This one needs a real
/// child because two of the guarantees are only observable with one: that
/// output is captured line by line off both streams while the process runs, and
/// that `Engine::stop` actually signals a deploy that is still going — which is
/// what the shutdown dialog's warning promises.
///
/// It runs `/bin/sh` twice: once printing three lines and exiting 0, once
/// sleeping. Nothing it does touches the network, Odoo, GitLab or any
/// repository.
///
/// Unix only, because a login shell is: see [`crate::platform::LOGIN_SHELL`]
/// and the test below, which pins what happens instead.
#[cfg(unix)]
#[test]
fn a_real_deploy_streams_its_output_and_engine_stop_kills_one_still_running() {
    let harness = engine_with(Setup {
        config: Some(json!({
            "deploy": {
                "projects": {
                    "Quick": { "command": "echo first; echo second >&2; echo third" },
                    "Slow": { "command": "sleep 30" },
                },
            },
        })),
        spawn: Some(SpawnPolicy::Allow),
        ..Setup::default()
    });

    // --- a run that finishes ---
    assert!(harness.engine.start_deploy("Quick").ok);
    let finished = wait_until(|| {
        harness
            .engine
            .deploy_run("Quick")
            .is_some_and(|run| !run.is_running())
    });
    assert!(finished, "the deploy never finished");

    let run = harness.engine.deploy_run("Quick").expect("a run");
    assert_eq!(run.status, DeployRunStatus::Ok);
    assert_eq!(run.exit_code, Some(0));
    assert_eq!(run.command, "echo first; echo second >&2; echo third");
    let captured = harness.engine.deploy_log("Quick");
    for expected in ["first", "second", "third"] {
        assert!(
            captured.iter().any(|line| line == expected),
            "{expected:?} missing from {captured:?}"
        );
    }
    assert!(
        captured.iter().any(|line| line == "second"),
        "stderr is captured too"
    );

    // --- and one that is still running when the engine stops ---
    harness.engine.start();
    assert!(harness.engine.start_deploy("Slow").ok);
    assert!(
        wait_until(|| harness.engine.deploy_run("Slow").is_some()),
        "the slow deploy never started"
    );
    harness.engine.stop();

    let killed = wait_until(|| {
        harness
            .engine
            .deploy_run("Slow")
            .is_some_and(|run| !run.is_running())
    });
    assert!(killed, "stopping the engine left a deploy running");
}

/// Poll a condition for a few seconds. A fixed sleep would be either flaky or
/// slow; this is both fast and patient.
#[cfg(unix)]
fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// And where there is no login shell, the refusal says so rather than running
/// the command under something that is not one.
///
/// A deploy command is written against the PATH a login shell sets up — `nvm`,
/// `asdf`, `gcloud`, `kubectl` all live behind profile setup — so running it
/// under `cmd /C` would not be the same deploy, it would be a different one
/// that looks like it worked.
#[cfg(not(unix))]
#[test]
fn a_deploy_is_refused_where_there_is_no_login_shell_to_run_it_under() {
    let harness = engine_with(Setup {
        config: Some(json!({
            "deploy": { "projects": { "Quick": { "command": "echo first" } } },
        })),
        spawn: Some(SpawnPolicy::Allow),
        ..Setup::default()
    });

    let result = harness.engine.start_deploy("Quick");
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some(crate::platform::unsupported("Deploying Quick").as_str())
    );
    // Refused, not started: nothing is left behind for the board to show.
    assert!(harness.engine.deploy_run("Quick").is_none());
}
