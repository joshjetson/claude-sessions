//! The gate. Asserted here and leaned on by every other suite in the crate.

use std::time::Duration;

use crate::term::{
    Exec, Iterm2Driver, LaunchRequest, SessionRef, SpawnPolicy, TerminalDriver, TmuxDriver,
};

#[test]
fn a_unit_test_binary_refuses_to_spawn_without_being_asked_to() {
    // `cfg!(test)` is true in here, which is the whole point: a test written by
    // someone who has never read this file still cannot launch an agent.
    assert_eq!(SpawnPolicy::detect(), SpawnPolicy::Refuse);
}

#[test]
fn a_refusal_names_what_it_refused() {
    let refused = SpawnPolicy::Refuse
        .check("open an ssh session")
        .expect_err("must refuse");
    assert!(refused
        .message
        .starts_with("Refusing to open an ssh session"));
    assert!(refused.message.contains("CLAUDE_SESSIONS_NO_SPAWN"));
}

#[test]
fn an_allowing_policy_lets_the_action_through() {
    assert!(SpawnPolicy::Allow.check("launch a session").is_ok());
    assert!(SpawnPolicy::Allow.is_allowed());
    assert!(!SpawnPolicy::Refuse.is_allowed());
}

#[test]
fn a_refused_command_never_reaches_the_operating_system() {
    // `true` exists on every machine this runs on, so a non-refusing
    // implementation would report ok here.
    let out = Exec::new(SpawnPolicy::Refuse).run("true", &[], Duration::from_secs(5));
    assert!(!out.ok);
    assert!(out.error.unwrap().starts_with("Refusing to run true"));
    assert!(out.stdout.is_empty());
}

#[test]
fn an_allowed_command_runs_and_reports_both_streams() {
    let out = Exec::new(SpawnPolicy::Allow).run(
        "sh",
        &["-c".to_string(), "echo out; echo err >&2".to_string()],
        Duration::from_secs(5),
    );
    assert!(out.ok, "{out:?}");
    assert_eq!(out.stdout, "out");
    assert_eq!(out.stderr, "err");
}

#[test]
fn a_missing_binary_is_a_verdict_not_a_panic() {
    let out = Exec::new(SpawnPolicy::Allow).run(
        "definitely-not-a-real-binary-xyz",
        &[],
        Duration::from_secs(5),
    );
    assert!(!out.ok);
    assert!(out.failure_message().contains("could not run"));
}

#[test]
fn both_drivers_refuse_rather_than_drive_a_terminal_from_a_test() {
    // This is the property the Node app learned the hard way: sixteen prompt
    // files and several live agents came out of test runs before the guard
    // existed.
    let policy = SpawnPolicy::detect();
    let request = LaunchRequest::new("/tmp", "claude").task_id(1);
    let session = SessionRef::from_tty("ttys004");

    for driver in [
        Box::new(TmuxDriver::new("cs", policy)) as Box<dyn TerminalDriver>,
        Box::new(Iterm2Driver::new(policy)),
    ] {
        assert!(!driver.launch(&request).ok, "{} launched", driver.name());
        assert!(
            !driver.send_text(&session, "hello").ok,
            "{} typed into a terminal",
            driver.name()
        );
        assert!(!driver.focus(&session).ok, "{} focused", driver.name());
        assert!(!driver.is_available(), "{} probed", driver.name());
    }
}
