//! `claude-sessions spawn`, with every outside answer faked.
//!
//! The host answers for the daemon, `ps`, tmux and Odoo, and the driver is the
//! board's [`RecordingDriver`], so nothing here can open a terminal even under
//! [`SpawnPolicy::Allow`]. The reviewer token is the one value that must never
//! reach the JSON line: several tests read it back from the database and look
//! for it there.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::cli::spawn::{
    live_task_windows, qa_flags, spawn, Outcome, SpawnContext, SpawnHost, SpawnInput,
    EXIT_BAD_INPUT, EXIT_CONFIG, EXIT_DRIVER, EXIT_FORBIDDEN, EXIT_NO_DAEMON, EXIT_OK,
    EXIT_RUNNING,
};
use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::{PendingRequest, Snapshot};
use crate::db::Db;
use crate::paths::Paths;
use crate::term::{
    DriverKind, DriverResult, LaunchRequest, SessionRef, SpawnPolicy, TerminalDriver,
    REVIEWER_TOKEN_ENV, TASK_ID_ENV,
};
use crate::types::Task;
use crate::ui::tests::board::fixtures::{live_session, RecordingDriver};

const TASK: i64 = 8123;
const ODOO: &str = "https://odoo.example.com";

/// A throwaway home, a config, and a repository folder to start in.
struct World {
    dir: TempDir,
    repo: TempDir,
    paths: Paths,
    config: ConfigHandle,
}

fn world_with(config: Value) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("repo");
    let path = dir.path().join(".claude-sessions.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    World {
        paths: Paths::for_test(dir.path()),
        config: ConfigHandle::load_from(&path, dir.path(), EnvOverrides::default()),
        dir,
        repo,
    }
}

fn world() -> World {
    world_with(json!({ "odoo": { "url": ODOO } }))
}

impl World {
    fn cwd(&self) -> String {
        self.repo.path().to_string_lossy().into_owned()
    }

    fn qa(&self) -> SpawnInput {
        SpawnInput {
            kind: Some("qa".into()),
            cwd: Some(self.cwd()),
            task_id: Some(TASK),
            title: Some(format!("qa-{TASK}-r1")),
            round: Some(1),
            prompt: Some("Note the verdict and move the stage.".into()),
            claude_flags: Some(format!(
                "--name qa-{TASK}-r1 --dangerously-skip-permissions --worktree qa-{TASK} \
                 --settings '{{\"env\":{{\"OPUS_SESSION\":\"qa-{TASK}-r1\"}}}}'"
            )),
            ..SpawnInput::default()
        }
    }

    fn token(&self) -> Option<String> {
        Db::open(&self.paths).reviewer_token(TASK)
    }

    fn prompt_files(&self) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&self.paths.prompts_dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }
}

/// The driver the host hands out: the board's recorder, plus what the token
/// row held at the moment `launch` was called.
struct ProbeDriver {
    recorder: Arc<RecordingDriver>,
    paths: Paths,
    token_at_launch: Mutex<Option<Option<String>>>,
}

impl TerminalDriver for ProbeDriver {
    fn name(&self) -> &'static str {
        "tmux"
    }
    fn is_available(&self) -> bool {
        true
    }
    fn launch(&self, request: &LaunchRequest) -> DriverResult {
        *self.token_at_launch.lock().unwrap() = Some(Db::open(&self.paths).reviewer_token(TASK));
        self.recorder.launch(request)
    }
    fn send_text(&self, session: &SessionRef, text: &str) -> DriverResult {
        self.recorder.send_text(session, text)
    }
    fn focus(&self, session: &SessionRef) -> DriverResult {
        self.recorder.focus(session)
    }
    fn close(&self, session: &SessionRef) -> DriverResult {
        self.recorder.close(session)
    }
}

/// Every outside answer, set per test.
struct FakeHost {
    daemon: Option<Snapshot>,
    outside: Vec<String>,
    driver: Arc<ProbeDriver>,
    pending: Mutex<Vec<PendingRequest>>,
    asked_daemon: Mutex<u32>,
    odoo_task: Option<Task>,
}

impl FakeHost {
    fn new(world: &World) -> Self {
        Self::with_recorder(world, RecordingDriver::shared())
    }

    fn with_recorder(world: &World, recorder: Arc<RecordingDriver>) -> Self {
        FakeHost {
            daemon: Some(Snapshot::default()),
            outside: Vec::new(),
            driver: Arc::new(ProbeDriver {
                recorder,
                paths: world.paths.clone(),
                token_at_launch: Mutex::new(None),
            }),
            pending: Mutex::new(Vec::new()),
            asked_daemon: Mutex::new(0),
            odoo_task: None,
        }
    }

    fn launched(&self) -> Vec<LaunchRequest> {
        self.driver.recorder.launched()
    }
}

impl SpawnHost for FakeHost {
    fn daemon_state(&self) -> Option<Snapshot> {
        *self.asked_daemon.lock().unwrap() += 1;
        self.daemon.clone()
    }
    fn post_pending(&self, pending: &PendingRequest) -> bool {
        self.pending.lock().unwrap().push(pending.clone());
        true
    }
    fn outside_sessions(&self, _task_id: i64) -> Vec<String> {
        self.outside.clone()
    }
    fn driver(&self, _requested: Option<DriverKind>) -> Option<Arc<dyn TerminalDriver>> {
        Some(self.driver.clone())
    }
    fn task(&self, _task_id: i64) -> Option<Task> {
        self.odoo_task.clone()
    }
    fn head_of(&self, _dir: &Path) -> Option<String> {
        None
    }
}

fn run_with(world: &World, host: &FakeHost, input: SpawnInput, policy: SpawnPolicy) -> Outcome {
    spawn(
        input,
        &SpawnContext {
            paths: &world.paths,
            config: &world.config,
            policy,
            port: 8787,
            host,
        },
    )
}

fn run(world: &World, host: &FakeHost, input: SpawnInput) -> Outcome {
    run_with(world, host, input, SpawnPolicy::Allow)
}

fn the_prompt(world: &World) -> String {
    let files = world.prompt_files();
    assert_eq!(files.len(), 1, "expected one prompt file: {files:?}");
    std::fs::read_to_string(&files[0]).unwrap()
}

// --- the request -------------------------------------------------------------

#[test]
fn a_request_with_no_cwd_is_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            cwd: None,
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_BAD_INPUT);
    assert_eq!(outcome.body["ok"], json!(false));
    assert!(outcome.body["error"].as_str().unwrap().contains("cwd"));
    assert!(host.launched().is_empty());
}

#[test]
fn a_cwd_that_is_not_a_folder_is_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    let missing = world
        .dir
        .path()
        .join("nowhere")
        .to_string_lossy()
        .into_owned();
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            cwd: Some(missing),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_BAD_INPUT);
}

#[test]
fn qa_without_a_task_id_is_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    for kind in ["qa", "qa-dry"] {
        let outcome = run(
            &world,
            &host,
            SpawnInput {
                kind: Some(kind.into()),
                task_id: None,
                title: None,
                ..world.qa()
            },
        );
        assert_eq!(outcome.code, EXIT_BAD_INPUT, "{kind}");
    }
    assert!(world.token().is_none());
}

#[test]
fn a_title_naming_another_task_is_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            title: Some("qa-9999-r1".into()),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_BAD_INPUT);
    assert!(world.token().is_none());
    assert!(world.prompt_files().is_empty());
}

#[test]
fn an_unknown_kind_is_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            kind: Some("task".into()),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_BAD_INPUT);
}

// --- the QA launch -----------------------------------------------------------

#[test]
fn a_qa_launch_sends_the_pipeline_prompt_not_the_one_opus_sent() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());

    let prompt = the_prompt(&world);
    assert!(
        prompt.contains(&format!("{ODOO}/web#id={TASK}&model=project.task")),
        "no task URL: {prompt}"
    );
    assert!(prompt.contains(&format!("Run /qa {TASK}")), "{prompt}");
    assert!(prompt.contains(REVIEWER_TOKEN_ENV), "no token clause");
    assert!(
        prompt.contains("When /qa reaches its end, print the frame"),
        "no park step"
    );
    assert!(
        !prompt.contains("move the stage"),
        "Opus's prompt leaked in"
    );
    assert_eq!(outcome.body["promptIgnored"], json!(true));
}

#[test]
fn a_qa_launch_on_a_recorded_revisions_round_opens_a_verification_round() {
    let world = world();
    let qa_dir = world.paths.qa_task_dir(TASK);
    std::fs::create_dir_all(&qa_dir).unwrap();
    std::fs::write(
        qa_dir.join("run.json"),
        r#"{"round":1,"meta":{"head":"7a2a9d1"},"cells":{"G1|check":{"verdict":"FAIL"}},"verdict":"revisions"}"#,
    )
    .unwrap();
    // Odoo answers with the task, so the only warning is the dropped flag.
    let mut host = FakeHost::new(&world);
    host.odoo_task = Some(crate::ui::tests::board::fixtures::task(
        TASK,
        "Fix the report",
    ));
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            title: Some(format!("qa-{TASK}-r2")),
            round: Some(2),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());
    let prompt = the_prompt(&world);
    assert!(prompt.contains("VERIFICATION round"), "{prompt}");
    assert!(prompt.contains("This launch opens round 2"), "{prompt}");
    assert_eq!(outcome.body["round"], json!(2));
    assert_eq!(outcome.body["priorRound"], json!(1));
    assert_eq!(outcome.body["priorVerdict"], json!("revisions"));
    assert_eq!(
        outcome.body["warnings"],
        json!([format!(
            "dropped --worktree qa-{TASK}: QAden makes its own worktree"
        )])
    );
}

#[test]
fn a_round_that_disagrees_with_run_json_warns_and_still_launches() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            title: Some(format!("qa-{TASK}-r3")),
            round: Some(3),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_OK);
    let warnings = outcome.body["warnings"].to_string();
    assert!(
        warnings.contains("round 3 asked for, but run.json makes this round 1"),
        "{warnings}"
    );
    assert_eq!(host.launched().len(), 1);
}

#[test]
fn the_token_row_is_written_before_the_terminal_opens_and_never_printed() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_OK);

    let token = world.token().expect("a QA launch records a token");
    assert_eq!(
        *host.driver.token_at_launch.lock().unwrap(),
        Some(Some(token.clone())),
        "the token row was not there when the terminal opened"
    );
    let launches = host.launched();
    assert_eq!(launches.len(), 1);
    assert_eq!(
        launches[0].env,
        vec![
            (TASK_ID_ENV.to_string(), TASK.to_string()),
            (REVIEWER_TOKEN_ENV.to_string(), token.clone()),
        ]
    );
    assert!(!outcome.line().contains(&token), "the token reached stdout");
    assert!(
        !the_prompt(&world).contains(&token),
        "the token reached the prompt file"
    );
    assert_eq!(outcome.body["tokenRecorded"], json!(true));
}

#[test]
fn a_qa_launch_keeps_the_bypass_and_settings_and_drops_the_worktree() {
    let world = world();
    let host = FakeHost::new(&world);
    run(&world, &host, world.qa());
    let command = &host.launched()[0].command;
    assert!(
        command.contains("'--dangerously-skip-permissions'"),
        "{command}"
    );
    assert!(
        command.contains(&format!(
            "'--settings' '{{\"env\":{{\"OPUS_SESSION\":\"qa-{TASK}-r1\"}}}}'"
        )),
        "{command}"
    );
    assert!(!command.contains("--worktree"), "{command}");
    assert_eq!(host.launched()[0].title.as_deref(), Some("qa-8123-r1"));
}

#[test]
fn the_pending_link_goes_to_the_daemon_with_the_sessions_already_there() {
    let world = world();
    let mut host = FakeHost::new(&world);
    let mut snapshot = Snapshot::default();
    snapshot
        .sessions
        .by_project
        .insert("p".into(), vec![live_session("other-task", Some(1), 10)]);
    host.daemon = Some(snapshot);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_OK);
    assert_eq!(
        *host.pending.lock().unwrap(),
        vec![PendingRequest {
            cwd: world.cwd(),
            task_id: Some(TASK),
            known_session_ids: vec!["other-task".into()],
        }]
    );
    assert_eq!(outcome.body["pendingPosted"], json!(true));
}

#[test]
fn the_output_keeps_the_keys_opus_reads() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(&world, &host, world.qa());
    for key in [
        "ok",
        "driver",
        "hint",
        "promptFile",
        "kind",
        "taskId",
        "title",
        "warnings",
    ] {
        assert!(
            outcome.body.contains_key(key),
            "missing {key}: {}",
            outcome.line()
        );
    }
    assert_eq!(outcome.body["ok"], json!(true));
    assert_eq!(outcome.body["driver"], json!("tmux"));
    let file = outcome.body["promptFile"].as_str().unwrap();
    assert!(file.contains(&format!("task-{TASK}-")), "{file}");
}

// --- refusals ----------------------------------------------------------------

#[test]
fn a_live_session_on_the_task_refuses_before_anything_is_written() {
    let world = world();
    let mut host = FakeHost::new(&world);
    let mut snapshot = Snapshot::default();
    snapshot.sessions.by_project.insert(
        "p".into(),
        vec![live_session("already-on-it", Some(TASK), 10)],
    );
    host.daemon = Some(snapshot);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_RUNNING, "{}", outcome.line());
    assert!(world.token().is_none(), "a token was written");
    assert!(world.prompt_files().is_empty());
    assert!(host.launched().is_empty());
    assert!(outcome.body["error"]
        .as_str()
        .unwrap()
        .contains("already-on-it"));

    // Force is the deliberate override.
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            force: true,
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_OK);
    assert!(world.token().is_some());
}

#[test]
fn a_process_or_window_outside_the_daemon_also_counts_as_live() {
    let world = world();
    let mut host = FakeHost::new(&world);
    host.outside = vec![format!("tmux window qa-{TASK}-r1")];
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_RUNNING);
    assert!(world.token().is_none());
}

#[test]
fn a_refusing_policy_writes_no_prompt_and_no_token() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run_with(&world, &host, world.qa(), SpawnPolicy::Refuse);
    assert_eq!(outcome.code, EXIT_FORBIDDEN);
    assert!(world.prompt_files().is_empty());
    assert!(world.token().is_none());
    assert!(host.launched().is_empty());
    assert!(host.pending.lock().unwrap().is_empty());
}

#[test]
fn qa_with_no_odoo_url_is_a_config_gap() {
    let world = world_with(json!({ "groups": [] }));
    let host = FakeHost::new(&world);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_CONFIG);
    assert!(world.token().is_none());
}

#[test]
fn qa_with_no_daemon_is_refused_and_a_dry_qa_only_warns() {
    let world = world();
    let mut host = FakeHost::new(&world);
    host.daemon = None;
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_NO_DAEMON);
    assert_eq!(
        outcome.body["error"],
        json!("no claude-sessions daemon on :8787")
    );
    assert!(world.token().is_none());
    assert!(host.launched().is_empty());

    let outcome = run(
        &world,
        &host,
        SpawnInput {
            kind: Some("qa-dry".into()),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());
    assert!(outcome.body["warnings"]
        .to_string()
        .contains("no claude-sessions daemon"));
    // The board gives a dry run no token, and so does this.
    assert!(world.token().is_none());
    assert_eq!(outcome.body["tokenRecorded"], json!(false));
}

#[test]
fn a_driver_failure_is_exit_1_with_the_token_left_in_place() {
    let world = world();
    let recorder = Arc::new(RecordingDriver {
        fail: true,
        ..RecordingDriver::default()
    });
    let host = FakeHost::with_recorder(&world, recorder);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_DRIVER);
    assert_eq!(outcome.body["ok"], json!(false));
    assert!(outcome.body["error"]
        .as_str()
        .unwrap()
        .contains("told to fail"));
    let token = world.token().expect("the token row stays");
    assert!(!outcome.line().contains(&token));
}

#[test]
fn a_dry_run_checks_everything_and_changes_nothing() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run_with(
        &world,
        &host,
        SpawnInput {
            dry_run: true,
            ..world.qa()
        },
        SpawnPolicy::Refuse,
    );
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());
    assert_eq!(outcome.body["dryRun"], json!(true));
    assert!(outcome.body["promptFile"]
        .as_str()
        .unwrap()
        .contains("task-8123-"));
    assert!(world.prompt_files().is_empty());
    assert!(world.token().is_none());
    assert!(host.launched().is_empty());
    assert!(host.pending.lock().unwrap().is_empty());
}

// --- the plain launch --------------------------------------------------------

#[test]
fn a_plain_launch_keeps_the_prompt_and_flags_verbatim_and_needs_no_daemon() {
    let world = world_with(json!({ "groups": [] }));
    let mut host = FakeHost::new(&world);
    host.daemon = None;
    let flags = "--dangerously-skip-permissions --name opus --worktree opus";
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            kind: Some("plain".into()),
            cwd: Some(world.cwd()),
            title: Some("opus".into()),
            prompt: Some("You are the master.".into()),
            claude_flags: Some(flags.into()),
            ..SpawnInput::default()
        },
    );
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());
    assert_eq!(
        *host.asked_daemon.lock().unwrap(),
        0,
        "plain asked the daemon"
    );
    assert_eq!(the_prompt(&world), "You are the master.");
    let launches = host.launched();
    assert!(
        launches[0]
            .command
            .starts_with(&format!("claude {flags} \"$(cat '")),
        "{}",
        launches[0].command
    );
    assert!(
        launches[0].env.is_empty(),
        "plain with no task exports nothing"
    );
    assert_eq!(launches[0].title.as_deref(), Some("opus"));
    assert!(world.token().is_none());
    assert_eq!(outcome.body["promptIgnored"], json!(false));
    assert_eq!(outcome.body["tokenRecorded"], json!(false));
}

#[test]
fn a_plain_launch_with_a_task_exports_the_task_id_and_no_token() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            kind: Some("plain".into()),
            cwd: Some(world.cwd()),
            task_id: Some(TASK),
            prompt: None,
            claude_flags: Some("--model opus".into()),
            ..SpawnInput::default()
        },
    );
    assert_eq!(outcome.code, EXIT_OK, "{}", outcome.line());
    let launches = host.launched();
    assert_eq!(launches[0].command, "claude --model opus");
    assert_eq!(
        launches[0].env,
        vec![(TASK_ID_ENV.to_string(), TASK.to_string())]
    );
    assert!(world.token().is_none());
}

// --- the flag filter ---------------------------------------------------------

#[test]
fn the_flag_filter_drops_every_worktree_spelling_and_keeps_the_rest() {
    let (kept, warnings) = qa_flags(
        "--name n --model opus -w a --worktree b --worktree=c --dangerously-skip-permissions \
         --settings '{\"env\":{\"OPUS_SESSION\":\"qa-1-r1\"}}' --add-dir /x",
    )
    .unwrap();
    assert_eq!(
        kept,
        "'--name' 'n' '--model' 'opus' '--dangerously-skip-permissions' '--settings' \
         '{\"env\":{\"OPUS_SESSION\":\"qa-1-r1\"}}' '--add-dir' '/x'"
    );
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[0].starts_with("dropped -w a"));
    assert!(warnings[1].starts_with("dropped --worktree b"));
    assert!(warnings[2].starts_with("dropped --worktree=c"));
}

#[test]
fn unbalanced_quotes_in_the_flags_are_bad_input() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(
        &world,
        &host,
        SpawnInput {
            claude_flags: Some("--settings '{".into()),
            ..world.qa()
        },
    );
    assert_eq!(outcome.code, EXIT_BAD_INPUT);
    assert!(world.token().is_none());
}

#[test]
fn a_task_odoo_cannot_describe_still_launches_with_a_warning() {
    let world = world();
    let host = FakeHost::new(&world);
    let outcome = run(&world, &host, world.qa());
    assert_eq!(outcome.code, EXIT_OK);
    assert!(outcome.body["warnings"]
        .to_string()
        .contains("Odoo did not answer"));
}

#[test]
fn a_dead_pane_does_not_count_as_a_live_session() {
    let listing = "task-7821 1\nqa-7821-r1 1\ntask-9 0\n";
    assert!(live_task_windows(listing, 7821).is_empty());
}

#[test]
fn a_running_pane_named_for_the_task_counts() {
    let listing = "task-7821 0\nqa-7821-r2 0\nqa-78210-r1 0\ntask-78 0\n";
    assert_eq!(
        live_task_windows(listing, 7821),
        vec!["task-7821".to_string(), "qa-7821-r2".to_string()]
    );
}
