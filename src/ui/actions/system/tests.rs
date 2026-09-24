//! `kill_pids` is the one runner here whose failure used to reach the screen:
//! `kill` inherited the terminal, and its exit code was ignored. These pin the
//! split between a refusal and a real failure, and that a failure carries what
//! `kill` said rather than printing it.

use super::*;

/// Far above any pid a real system hands out: macOS stops at 99998 and Linux
/// at 4194304. Signalling it can only fail, so this test never touches a real
/// process.
const NO_SUCH_PID: u32 = 2_000_000_000;

#[test]
fn an_empty_pid_list_is_a_refusal() {
    assert_eq!(
        kill_pids(&[], SpawnPolicy::Allow),
        Err(KillError::Refused("Nothing to kill.".to_string()))
    );
}

#[test]
fn the_spawn_gate_is_a_refusal_not_an_error() {
    // Native Windows refuses earlier, for its own reason. Either way nothing
    // ran, so it is a refusal and never an error.
    let result = kill_pids(&[NO_SUCH_PID], SpawnPolicy::Refuse);
    assert!(matches!(result, Err(KillError::Refused(_))), "{result:?}");
}

#[cfg(unix)]
#[test]
fn a_failed_kill_is_an_error_with_what_kill_said() {
    let result = kill_pids(&[NO_SUCH_PID], SpawnPolicy::Allow);
    let Err(KillError::Failed(message)) = result else {
        panic!("a kill of a pid that cannot exist must fail, got {result:?}");
    };
    assert!(message.contains("kill -TERM"), "{message}");
    assert!(message.contains(&NO_SUCH_PID.to_string()), "{message}");
    assert!(message.contains("exited with"), "{message}");
}

#[test]
fn a_failure_sentence_includes_stderr_only_when_there_is_some() {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(1 << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(1)
    };
    let quiet = failure("kill -TERM", "42", &status, b"  \n");
    assert!(quiet.starts_with("kill -TERM 42 exited with"), "{quiet}");
    assert!(!quiet.ends_with(": "), "{quiet}");
    let loud = failure("kill -TERM", "42", &status, b"kill: 42: No such process\n");
    assert!(loud.ends_with(": kill: 42: No such process"), "{loud}");
}
