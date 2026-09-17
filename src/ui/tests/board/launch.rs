//! What actually happens when a launch reaches the worker.
//!
//! Two layers, deliberately:
//!
//! 1. [`SpawnPolicy::Refuse`] — the anti-spawn guard. A test that walked past
//!    the start gate in the Node app reached the real launch path, found the
//!    user's live project-to-directory mapping, and opened terminal tabs
//!    running `claude --dangerously-skip-permissions` against production task
//!    data. Sixteen prompt files came out of test runs before anyone noticed.
//! 2. [`SpawnPolicy::Allow`] with a recording driver, to assert the shape of
//!    what WOULD be run. No process can start: the driver records instead of
//!    driving, so both layers have to fail before a terminal opens.

use std::sync::mpsc::channel;
use std::sync::Arc;

use crate::term::{SpawnPolicy, TerminalDriver, TASK_ID_ENV};
use crate::ui::actions::{ActionResult, BoardServices};
use crate::ui::board::{start, LaunchKind, LaunchSpec};
use crate::ui::state::Action;

use super::fixtures::*;

/// A spec, and the temporary runtime it writes into.
fn spec_for(dir: &tempfile::TempDir, prompt: Option<&str>) -> LaunchSpec {
    LaunchSpec {
        task_id: 5238,
        cwd: dir.path().to_string_lossy().into_owned(),
        flags: "--dangerously-skip-permissions".to_string(),
        prompt: prompt.map(str::to_string),
        title: "task-5238".to_string(),
        stage_move: None,
        known_session_ids: vec!["already-here".to_string()],
        say: String::new(),
    }
}

pub(super) struct Harness {
    _dir: tempfile::TempDir,
    pub(super) services: BoardServices,
    pub(super) driver: Arc<super::fixtures::RecordingDriver>,
}

pub(super) fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(dir.path());
    Harness {
        services: BoardServices::offline(paths),
        driver: RecordingDriver::shared(),
        _dir: dir,
    }
}

fn run(harness: &Harness, spec: &LaunchSpec, policy: SpawnPolicy) -> Vec<ActionResult> {
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::board::launch(spec, &harness.services, &driver, policy, &tx);
    drop(tx);
    rx.into_iter().collect()
}

#[test]
fn a_refusing_policy_opens_no_terminal_and_writes_no_prompt() {
    let harness = harness();
    let repo = tempfile::tempdir().expect("repo");
    let spec = spec_for(&repo, Some("do the thing"));
    let results = run(&harness, &spec, SpawnPolicy::Refuse);

    assert!(
        harness.driver.launched().is_empty(),
        "a terminal was opened"
    );
    assert!(
        !harness.services.paths.prompts_dir.exists(),
        "a prompt file was written under a refusing policy"
    );
    let said = format!("{results:?}");
    assert!(said.contains("Refusing to launch a session"), "{said}");
}

#[test]
fn a_launch_writes_the_prompt_to_a_file_and_runs_claude_against_it() {
    // The prompt goes through a file: it is far too long for a command line,
    // and embedding it would mean escaping it for both the shell and the
    // terminal driver.
    let harness = harness();
    let repo = tempfile::tempdir().expect("repo");
    let spec = spec_for(&repo, Some("do the thing"));
    run(&harness, &spec, SpawnPolicy::Allow);

    let written: Vec<_> = std::fs::read_dir(&harness.services.paths.prompts_dir)
        .expect("prompts dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(written.len(), 1, "expected exactly one prompt file");
    let name = written[0].file_name().to_string_lossy().into_owned();
    assert!(name.starts_with("task-5238-"), "{name}");
    assert_eq!(
        std::fs::read_to_string(written[0].path()).unwrap(),
        "do the thing"
    );

    let launches = harness.driver.launched();
    assert_eq!(launches.len(), 1);
    assert!(
        launches[0]
            .command
            .contains("claude --dangerously-skip-permissions \"$(cat '"),
        "{}",
        launches[0].command
    );
    assert!(
        launches[0].command.contains(&name),
        "{}",
        launches[0].command
    );
}

#[test]
fn a_launch_exports_the_task_id_and_names_the_tab() {
    // The environment variable is how the scanner recovers process -> task, and
    // how `claude-sessions done` knows what it finished.
    let harness = harness();
    let repo = tempfile::tempdir().expect("repo");
    run(&harness, &spec_for(&repo, Some("x")), SpawnPolicy::Allow);
    let launches = harness.driver.launched();
    assert_eq!(
        launches[0].env,
        vec![(TASK_ID_ENV.to_string(), "5238".to_string())]
    );
    assert_eq!(launches[0].title.as_deref(), Some("task-5238"));
}

#[test]
fn the_summaries_directory_exists_before_the_agent_is_told_to_write_there() {
    // A missing directory turns a finished task into a silent one.
    let harness = harness();
    let repo = tempfile::tempdir().expect("repo");
    assert!(!harness.services.paths.summaries_dir.exists());
    run(&harness, &spec_for(&repo, Some("x")), SpawnPolicy::Allow);
    assert!(harness.services.paths.summaries_dir.is_dir());
}

#[test]
fn a_promptless_launch_opens_an_empty_session() {
    let harness = harness();
    let repo = tempfile::tempdir().expect("repo");
    let mut spec = spec_for(&repo, None);
    spec.flags = "--resume abc --dangerously-skip-permissions".to_string();
    run(&harness, &spec, SpawnPolicy::Allow);
    let launches = harness.driver.launched();
    assert_eq!(
        launches[0].command,
        "claude --resume abc --dangerously-skip-permissions"
    );
    assert!(
        !harness.services.paths.prompts_dir.exists(),
        "the promptless path wrote a prompt file"
    );
}

#[test]
fn a_launch_is_registered_with_the_pending_queue_before_the_terminal_opens() {
    // A `claude` process writes no transcript for its first seconds, so a
    // launch that is not queued by then is a session nothing will ever claim.
    let feed = crate::ui::feed::StaticFeed::default();
    let repo = tempfile::tempdir().expect("repo");
    let action = Action::Launch(Box::new(spec_for(&repo, Some("x"))));
    crate::ui::board::note_launch(&feed, &action);
    assert_eq!(
        feed.launches.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the launch was not announced to the feed"
    );
}

#[test]
fn the_launch_spec_carries_the_sessions_that_already_existed() {
    // So the linker can tell the new session apart from them.
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    with_sessions(&mut state, vec![live_session("existing", None, 1000)]);
    start(
        &mut state,
        start_request(&task(5238, "x"), LaunchKind::Task),
    );
    let queued = actions(&mut state);
    let Some(Action::Launch(spec)) = queued.into_iter().find(|a| matches!(a, Action::Launch(_)))
    else {
        panic!("no launch");
    };
    assert_eq!(spec.known_session_ids, vec!["existing".to_string()]);
    assert_eq!(spec.cwd, "/tmp/repo");
}

#[test]
fn a_task_launch_asks_for_the_working_stage_and_a_qa_launch_does_not() {
    // QA does not move the stage: the QA stage IS the working stage, and a live
    // session already shows on the row.
    let (_dir, mut state) = board_state();
    state
        .config
        .add_odoo_project_dir("NoSuchProject-ForTests", "/tmp/repo")
        .unwrap();
    for (kind, expect_move) in [
        (LaunchKind::Task, true),
        (LaunchKind::Qa, false),
        (LaunchKind::QaDry, false),
        (LaunchKind::PreOptics, false),
    ] {
        start(&mut state, start_request(&task(5238, "x"), kind.clone()));
        let queued = actions(&mut state);
        let Some(Action::Launch(spec)) =
            queued.into_iter().find(|a| matches!(a, Action::Launch(_)))
        else {
            panic!("no launch for {kind:?}");
        };
        assert_eq!(
            spec.stage_move.is_some(),
            expect_move,
            "{kind:?} stage move"
        );
    }
}

#[test]
fn qa_does_not_skip_permission_prompts() {
    // QA drives real preview environments carrying live credentials, so a
    // send-shaped button stops and asks rather than clicking.
    assert_eq!(LaunchKind::Qa.flags(), "");
    assert_eq!(LaunchKind::QaDry.flags(), "");
    assert_eq!(LaunchKind::PreOptics.flags(), "");
    assert_eq!(LaunchKind::Task.flags(), "--dangerously-skip-permissions");
}

#[test]
fn a_driver_failure_is_reported_rather_than_swallowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(dir.path());
    let services = BoardServices::offline(paths);
    let driver: Arc<dyn TerminalDriver> = Arc::new(RecordingDriver {
        fail: true,
        ..RecordingDriver::default()
    });
    let repo = tempfile::tempdir().expect("repo");
    let (tx, rx) = channel();
    crate::ui::actions::board::launch(
        &spec_for(&repo, Some("x")),
        &services,
        &driver,
        SpawnPolicy::Allow,
        &tx,
    );
    drop(tx);
    let said = format!("{:?}", rx.into_iter().collect::<Vec<_>>());
    assert!(
        said.contains("Couldn't open a terminal for task 5238"),
        "{said}"
    );
}

#[test]
fn a_revision_is_typed_into_the_session_and_then_focused() {
    // Go there, so you can see it land and answer anything it asks.
    let harness = harness();
    let spec = crate::ui::board::SendSpec {
        task_id: 7777,
        session: crate::term::SessionRef::from_tty("ttys004"),
        session_id: "live-abc12345".to_string(),
        prompt: "handle the revision".to_string(),
        stage_move: None,
    };
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::board::send_to_session(
        &spec,
        &harness.services,
        &driver,
        SpawnPolicy::Allow,
        &tx,
    );
    drop(tx);
    let said = format!("{:?}", rx.into_iter().collect::<Vec<_>>());

    let sends = harness.driver.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].1, "handle the revision");
    assert_eq!(harness.driver.focus_count(), 1);
    assert!(said.contains("Revision sent to session live-abc"), "{said}");
}

#[test]
fn a_refusing_policy_types_into_nothing() {
    let harness = harness();
    let spec = crate::ui::board::SendSpec {
        task_id: 7777,
        session: crate::term::SessionRef::from_tty("ttys004"),
        session_id: "live-abc12345".to_string(),
        prompt: "handle the revision".to_string(),
        stage_move: None,
    };
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::board::send_to_session(
        &spec,
        &harness.services,
        &driver,
        SpawnPolicy::Refuse,
        &tx,
    );
    drop(tx);
    let said = format!("{:?}", rx.into_iter().collect::<Vec<_>>());
    assert!(harness.driver.sends().is_empty());
    assert_eq!(harness.driver.focus_count(), 0);
    assert!(said.contains("Refusing to type into session"), "{said}");
}

#[test]
fn resuming_a_task_with_no_archive_says_so_and_opens_nothing() {
    let harness = harness();
    let request = crate::ui::board::ResumeRequest {
        prompt: crate::ui::board::PromptContext {
            task_id: 991001,
            ..Default::default()
        },
        revision: false,
        link_cwd: String::new(),
        stage_move: None,
        known_session_ids: Vec::new(),
    };
    let (tx, rx) = channel();
    let driver: Arc<dyn TerminalDriver> = harness.driver.clone();
    crate::ui::actions::board::resume(
        &request,
        &harness.services,
        &driver,
        SpawnPolicy::Allow,
        &tx,
    );
    drop(tx);
    let said = format!("{:?}", rx.into_iter().collect::<Vec<_>>());
    assert!(
        said.contains("No conversation archived for #991001"),
        "{said}"
    );
    assert!(harness.driver.launched().is_empty());
}
