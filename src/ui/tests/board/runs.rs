//! Watching a stage as a QA run, from the board.
//!
//! These are the assertions about what the KEYS do. What a run row says lives
//! in `board::tests::qarun`; what a run decides lives in `qarun::tests`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::fixtures::{board_state, task, with_tasks};
use crate::board::BoardItem;
use crate::qarun::RunMode;
use crate::ui::board::{self, BoardRow};
use crate::ui::state::View;

fn press(state: &mut crate::ui::state::AppState, ch: char) {
    board::handle_board(state, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
}

fn down(state: &mut crate::ui::state::AppState) {
    board::handle_board(state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
}

/// A board with three tasks in one stage, the cursor on the stage row.
fn on_a_stage() -> (tempfile::TempDir, crate::ui::state::AppState) {
    let (dir, mut state) = board_state();
    state.view = View::Board;
    with_tasks(
        &mut state,
        vec![
            task(4101, "Summary row shows the wrong total"),
            task(4102, "Description field ignores its length cap"),
            task(4103, "Number formatting differs between panels"),
        ],
    );
    // Open the project so the stage row exists, then land on it.
    let snapshot = board::snapshot(&state);
    if let BoardRow::Project { .. } = snapshot.row {
        board::handle_board(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );
        down(&mut state);
    }
    (dir, state)
}

#[test]
fn r_on_a_stage_starts_watching_it() {
    let (_dir, mut state) = on_a_stage();
    assert!(matches!(
        board::snapshot(&state).row,
        BoardRow::Stage { .. }
    ));

    press(&mut state, 'R');

    assert_eq!(state.board.runs.len(), 1, "no run was created");
    let run = &state.board.runs[0];
    assert_eq!(run.task_ids.len(), 3);
    // Triage is the default. A coordinator that answers nothing leaves every
    // question with the reviewer, which is the job it exists to take on.
    assert_eq!(run.mode, RunMode::Triage);
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("3 tasks")));
}

#[test]
fn watching_the_same_stage_twice_does_not_make_two_runs() {
    // Two runs over the same tasks would each claim the rows, and the board
    // would draw them twice.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();

    // Move back onto the stage row and press again.
    state.board.stop_watching("nothing");
    let before = state.board.runs.len();
    state.board.watch_stage("Aurora", "Approved to Start");
    assert_eq!(state.board.runs.len(), before);
    assert_eq!(state.board.runs[0].id, run_id);
}

#[test]
fn a_run_keeps_a_task_the_stage_has_lost() {
    // The reason a run is a record and not a view over the stage.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    assert_eq!(state.board.runs[0].task_ids.len(), 3);

    // The stage now holds one task; the run still covers all three.
    with_tasks(
        &mut state,
        vec![task(4101, "Summary row shows the wrong total")],
    );
    state.board.watch_stage("Aurora", "Approved to Start");
    assert_eq!(
        state.board.runs[0].task_ids.len(),
        3,
        "a task that left the stage was dropped from the run"
    );
}

#[test]
fn a_run_puts_its_rows_on_the_board() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    assert!(
        snapshot.keys.iter().any(|key| key.starts_with("br:")),
        "no run header row: {:?}",
        snapshot.keys
    );
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|key| key.starts_with("brt:"))
            .count(),
        3
    );
    // …and its tasks are no longer loose rows underneath it.
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|key| key.starts_with("bt:"))
            .count(),
        0,
        "run tasks also appeared as loose stage rows"
    );
}

#[test]
fn r_on_a_run_row_stops_watching() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    assert_eq!(state.board.runs.len(), 1);

    // Land on the run header and press again.
    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    press(&mut state, 'R');

    assert!(state.board.runs.is_empty(), "the run was not dropped");
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("untouched")));
}

#[test]
fn r_on_a_row_that_is_neither_says_so() {
    let (_dir, mut state) = board_state();
    press(&mut state, 'R');
    assert!(state.board.runs.is_empty());
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("Select a stage")));
}

#[test]
fn the_jump_key_says_so_when_nothing_is_waiting() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    press(&mut state, ']');
    assert!(state
        .flash
        .as_deref()
        .is_some_and(|f| f.contains("No agent is waiting")));
}

#[test]
fn enter_on_a_run_header_opens_the_run_menu() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    assert!(
        matches!(state.dialog, Some(crate::ui::dialogs::Dialog::RunMenu(_))),
        "the run menu did not open"
    );
}

#[test]
fn collapsing_a_run_hides_its_rows_without_releasing_its_tasks() {
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(&mut state, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    let after = board::snapshot(&state);
    assert_eq!(
        after.keys.iter().filter(|k| k.starts_with("brt:")).count(),
        0,
        "collapsing left the run's rows on screen"
    );
    assert_eq!(
        after.keys.iter().filter(|k| k.starts_with("bt:")).count(),
        0,
        "collapsing released the run's tasks back to the stage"
    );
}

#[test]
fn a_run_row_is_still_a_task_row() {
    // Every key that works on a board task works here: that is most of the
    // argument for putting runs on the board rather than in their own tab.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let first = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("brt:"))
        .expect("a run task row");
    state.board_sel.set(&snapshot.keys, first);

    let row = board::snapshot(&state).row;
    assert!(matches!(row, BoardRow::QaRunTask { task: Some(_), .. }));
    assert!(row.task().is_some(), "a run row did not answer as a task");
}

#[test]
fn the_board_is_unchanged_when_nothing_is_watched() {
    let (_dir, mut state) = on_a_stage();
    // Open the stage by hand: watching one opens it as a side effect, and this
    // test is about the board WITHOUT a run.
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );

    let snapshot = board::snapshot(&state);
    assert!(
        !snapshot.keys.iter().any(|key| key.starts_with("br")),
        "a run row appeared with no run: {:?}",
        snapshot.keys
    );
    assert_eq!(
        snapshot
            .keys
            .iter()
            .filter(|k| k.starts_with("bt:"))
            .count(),
        3,
        "the ordinary task rows changed: {:?}",
        snapshot.keys
    );
    let _ = BoardItem::Separator;
}

#[test]
fn the_run_menu_offers_a_context_prompt() {
    // The JS original lets you type something for the coordinator before it
    // starts. This is that entry.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    let Some(crate::ui::dialogs::Dialog::RunMenu(menu)) = &state.dialog else {
        panic!("the run menu did not open");
    };
    assert!(
        menu.entries
            .iter()
            .any(|(_, action)| *action == crate::ui::dialogs::RunAction::StartRunWithContext),
        "the run menu has no context entry: {:?}",
        menu.entries
            .iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    );
}

#[test]
fn typed_context_reaches_the_run_launch() {
    // The context prompt hands back a StartRun carrying the text, so the
    // launch path needs no second entry point.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();

    let mut prompt = crate::ui::dialogs::RunContext::new(&run_id, "Approved to Start");
    let mut config = state.config.clone();
    let mut ctx = crate::ui::dialogs::DialogCtx {
        config: &mut config,
    };
    for ch in "preview is down".chars() {
        prompt.handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut ctx,
        );
    }
    // Multiline, so Enter is a newline and Ctrl-S submits — the same contract
    // the task context dialog uses.
    let outcome = prompt.handle_key(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        &mut ctx,
    );

    match outcome {
        crate::ui::dialogs::DialogOutcome::Run(command) => {
            assert_eq!(command.run_id, run_id);
            assert_eq!(command.action, crate::ui::dialogs::RunAction::StartRun);
            assert_eq!(command.context, "preview is down");
        }
        other => panic!("expected a run command, got {other:?}"),
    }
}

// --- one button, and what it does --------------------------------------------

#[test]
fn the_run_menu_offers_exactly_one_way_to_start() {
    // Two peer entries — "Start QA sessions" and "Start coordinator" — read as
    // a choice between them, and buried the fact that the coordinator is the
    // layer the QA sessions ask their questions through. A run without one
    // sends every question to the reviewer, which is the job it exists to take
    // on. So there is one start, and it starts both.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    let snapshot = board::snapshot(&state);
    let header = snapshot
        .keys
        .iter()
        .position(|key| key.starts_with("br:"))
        .expect("run header");
    state.board_sel.set(&snapshot.keys, header);
    board::handle_board(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    let Some(crate::ui::dialogs::Dialog::RunMenu(menu)) = &state.dialog else {
        panic!("the run menu did not open");
    };
    let starts = menu
        .entries
        .iter()
        .filter(|(_, action)| *action == crate::ui::dialogs::RunAction::StartRun)
        .count();
    assert_eq!(
        starts,
        1,
        "expected one start entry, got {starts}: {:?}",
        menu.entries
            .iter()
            .map(|(label, _)| label)
            .collect::<Vec<_>>()
    );
    // And no entry that starts a coordinator on its own, or flips the mode:
    // the mode is config, so one run cannot mix two agreement numbers.
    let labels: Vec<&String> = menu.entries.iter().map(|(label, _)| label).collect();
    assert!(
        !labels.iter().any(|label| label.contains("Switch to")),
        "the mode toggle is still in the menu: {labels:?}"
    );
}

#[test]
fn starting_a_run_twice_does_not_start_a_second_coordinator() {
    // Two coordinators would both triage the same questions, and each would be
    // told about every one of them.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();

    // Stand in for the launch having gone out.
    state.board.runs[0].coordinator_started = true;

    board::run_command(
        &mut state,
        crate::ui::dialogs::RunCommand {
            run_id: run_id.clone(),
            action: crate::ui::dialogs::RunAction::StartRun,
            context: String::new(),
        },
    );

    // Nothing new was launched: no Launch action reached the queue.
    assert!(
        !state
            .take_actions()
            .iter()
            .any(|action| matches!(action, crate::ui::state::Action::Launch(_))),
        "a second coordinator launch went out"
    );
}

#[test]
fn a_question_on_a_run_task_wakes_that_runs_coordinator() {
    // The coordinator finishes its launch turn and then sits idle. Without
    // this prod it never learns a QA session asked anything, and the whole
    // interface is a session that watched nothing.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    state.board.runs[0].coordinator_started = true;
    let run_id = state.board.runs[0].id.clone();
    state.by_project.insert(
        "alpha".to_string(),
        vec![coordinator_session("coord", "/repo/alpha", Some(&run_id))],
    );

    state.push_notification(question_for(4101));

    assert!(
        state
            .take_actions()
            .iter()
            .any(|action| matches!(action, crate::ui::state::Action::NudgeCoordinator(_))),
        "no nudge was queued for a question on a run task"
    );
}

#[test]
fn a_verdict_checkpoint_never_wakes_the_coordinator() {
    // A verdict is the one thing that stays with the reviewer in every mode.
    // Telling the coordinator about it invites it to try, and the answer policy
    // would then refuse — a wasted turn that teaches it nothing.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    state.board.runs[0].coordinator_started = true;
    let run_id = state.board.runs[0].id.clone();
    state.by_project.insert(
        "alpha".to_string(),
        vec![coordinator_session("coord", "/repo/alpha", Some(&run_id))],
    );

    let mut verdict = question_for(4101);
    verdict.kind = crate::types::NotificationKind::Verdict;
    state.push_notification(verdict);

    assert!(
        !state
            .take_actions()
            .iter()
            .any(|action| matches!(action, crate::ui::state::Action::NudgeCoordinator(_))),
        "a verdict checkpoint was sent to the coordinator"
    );
}

#[test]
fn a_run_with_no_coordinator_queues_no_nudge() {
    // Silent on purpose: the run row already says it has no coordinator, and a
    // toast per question would bury it.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');

    state.push_notification(question_for(4101));

    assert!(
        !state
            .take_actions()
            .iter()
            .any(|action| matches!(action, crate::ui::state::Action::NudgeCoordinator(_))),
        "a nudge was queued with no coordinator to receive it"
    );
}

fn question_for(task_id: i64) -> crate::types::Notification {
    crate::types::Notification {
        id: format!("q{task_id}"),
        title: "which environment?".to_string(),
        message: String::new(),
        cwd: String::new(),
        project: String::new(),
        session_id: None,
        task_id: Some(task_id),
        level: crate::types::NotificationLevel::Warn,
        kind: crate::types::NotificationKind::Question,
        ts: String::new(),
        status: crate::types::NotificationStatus::Unread,
    }
}

fn coordinator_session(id: &str, cwd: &str, run: Option<&str>) -> crate::types::Session {
    crate::types::Session {
        session_id: id.to_string(),
        pids: vec![1],
        cwd: cwd.to_string(),
        tty: Some("ttys001".to_string()),
        lstart: None,
        session_file: Some(std::path::PathBuf::from("/t.jsonl")),
        session_mtime: std::time::SystemTime::UNIX_EPOCH,
        session_size: None,
        status: crate::types::SessionStatus::Idle,
        activity_detail: String::new(),
        starting: false,
        git_branch: None,
        last_timestamp: None,
        last_usage: None,
        last_entry: None,
        cumulative_usage: None,
        prompts: Vec::new(),
        task_id: None,
        run_id: run.map(str::to_string),
    }
}

// --- a run survives quitting the dashboard -----------------------------------

#[test]
fn a_watched_run_is_still_there_after_a_restart() {
    // The failure this fixes: a run lived in memory only, so quitting the TUI
    // ended it. The coordinator and its agents kept running, but the dashboard
    // no longer knew they belonged together — every one of them fell back to
    // its project and the Runs section vanished.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(dir.path());
    let config = crate::config::ConfigHandle::load(&paths, crate::config::EnvOverrides::default());

    let watched = {
        let mut state = crate::ui::state::AppState::new(paths.clone(), config.clone());
        state.view = View::Board;
        with_tasks(
            &mut state,
            vec![
                task(4101, "Summary row shows the wrong total"),
                task(4102, "Description field ignores its length cap"),
            ],
        );
        let snapshot = board::snapshot(&state);
        if let BoardRow::Project { .. } = snapshot.row {
            board::handle_board(
                &mut state,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            );
            down(&mut state);
        }
        press(&mut state, 'R');
        assert_eq!(state.board.runs.len(), 1, "no run to persist");
        state.board.runs[0].clone()
    };

    // A second AppState over the same paths is what reopening the TUI is.
    let reopened = crate::ui::state::AppState::new(paths, config);
    assert_eq!(
        reopened.board.runs,
        vec![watched],
        "the run did not survive the restart"
    );
}

#[test]
fn stopping_a_run_stops_it_across_a_restart_too() {
    // Otherwise a dismissed run comes back the next time you open the app,
    // which is worse than never having persisted it.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = crate::paths::Paths::for_test(dir.path());
    let config = crate::config::ConfigHandle::load(&paths, crate::config::EnvOverrides::default());

    {
        let mut state = crate::ui::state::AppState::new(paths.clone(), config.clone());
        state.view = View::Board;
        with_tasks(
            &mut state,
            vec![task(4101, "Summary row shows the wrong total")],
        );
        let snapshot = board::snapshot(&state);
        if let BoardRow::Project { .. } = snapshot.row {
            board::handle_board(
                &mut state,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            );
            down(&mut state);
        }
        press(&mut state, 'R');
        let run_id = state.board.runs[0].id.clone();
        board::run_command(
            &mut state,
            crate::ui::dialogs::RunCommand {
                run_id,
                action: crate::ui::dialogs::RunAction::StopWatching,
                context: String::new(),
            },
        );
        assert!(state.board.runs.is_empty());
    }

    let reopened = crate::ui::state::AppState::new(paths, config);
    assert!(
        reopened.board.runs.is_empty(),
        "a stopped run came back: {:?}",
        reopened.board.runs
    );
}

// --- the coordinator's prompt actually carries its run -----------------------

/// Render a coordinator prompt the way a launch does.
fn coordinator_prompt(extras: &[(&str, &str)]) -> String {
    let (_dir, state) = board_state();
    let task = task(4101, "Summary row shows the wrong total");
    let extras: std::collections::BTreeMap<String, String> = extras
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::ui::board::prompt_context(
        &state.config,
        &state.paths,
        &task,
        String::new(),
        "",
        &extras,
    )
    .prompt(&crate::ui::board::LaunchKind::QaRun, "/repo", None)
    .expect("a rendered prompt")
    .expect("a prompt, not an empty one")
}

#[test]
fn the_coordinator_prompt_names_its_run_and_its_tasks() {
    // The failure this pins was quiet and expensive. `start_coordinator` set
    // runId, taskIds, qaRoot and triage on the request; `prompt_context` built
    // its extras from the task alone and never read them. The coordinator then
    // rendered "this run" and "(none listed)", could not tell which tasks were
    // its own, guessed from file timestamps, and reported eleven tasks for a
    // run of seven. Nothing errored — it did the wrong job carefully.
    let prompt = coordinator_prompt(&[
        ("runId", "Aurora::Quality Assurance"),
        ("taskIds", "4101, 4102, 4103"),
        ("qaRoot", "/qa"),
    ]);

    assert!(
        prompt.contains("Aurora::Quality Assurance"),
        "the prompt does not name the run: {prompt}"
    );
    // The placeholder, not the words: "for any task in this run" is ordinary
    // prose elsewhere in the prompt and must not fail this.
    assert!(
        !prompt.contains(r#"QA run "this run""#),
        "the run id fell back to its placeholder: {prompt}"
    );
    assert!(
        !prompt.contains("(none listed)"),
        "the task list did not reach the prompt: {prompt}"
    );
    for id in ["4101", "4102", "4103"] {
        assert!(prompt.contains(id), "task {id} is missing: {prompt}");
    }
    assert!(
        prompt.contains("/qa"),
        "the QA directory did not reach the prompt: {prompt}"
    );
}

#[test]
fn a_triage_run_gives_its_coordinator_the_triage_rules() {
    // Without the `triage` variable the prompt took its SHADOW branch and told
    // the coordinator to answer nothing, while the run's own mode said triage.
    // The two disagreed, and only the prompt was load-bearing.
    let prompt = coordinator_prompt(&[("runId", "r"), ("taskIds", "4101"), ("triage", "true")]);
    assert!(
        prompt.contains("you triage it"),
        "a triage run got the shadow rules: {prompt}"
    );
    assert!(
        !prompt.contains("you do NOT answer it"),
        "the shadow branch is still in a triage prompt: {prompt}"
    );
}

#[test]
fn a_shadow_run_still_gets_the_shadow_rules() {
    // The other half: shadow must not quietly become triage now that the
    // variable is actually read.
    let prompt = coordinator_prompt(&[("runId", "r"), ("taskIds", "4101")]);
    assert!(
        prompt.contains("you do NOT answer it"),
        "shadow mode lost its rules: {prompt}"
    );
}

// --- a coordinator is never one of the agents --------------------------------

#[test]
fn the_coordinator_does_not_take_an_agents_row() {
    // Observed on screen: two rows for task #6661. The coordinator sat in the
    // run's agent slot for that task and the real reviewer was pushed out into
    // its project, so one task had two rows and neither was right.
    //
    // The coordinator launches against the run's FIRST task only to resolve a
    // folder, which is how it came to be treated as that task's session.
    let (_dir, mut state) = on_a_stage();
    press(&mut state, 'R');
    let run_id = state.board.runs[0].id.clone();
    let first = state.board.runs[0].task_ids[0];

    let mut coordinator = coordinator_session("coord", "/repo/alpha", Some(&run_id));
    // The thing that caused it: the coordinator carrying the run's first task.
    coordinator.task_id = Some(first);
    let mut agent = coordinator_session("agent", "/repo/alpha", None);
    agent.task_id = Some(first);

    state
        .by_project
        .insert("alpha".to_string(), vec![coordinator, agent]);

    let sections = state.run_sections();
    let section = sections.iter().find(|s| s.run_id == run_id).expect("run");

    assert_eq!(
        section.coordinator.map(|s| s.session_id.as_str()),
        Some("coord"),
        "the coordinator was not recognised"
    );
    let row = section
        .agents
        .iter()
        .find(|agent| agent.task_id == first)
        .expect("a row for the first task");
    assert_eq!(
        row.session.map(|s| s.session_id.as_str()),
        Some("agent"),
        "the coordinator took the agent's row"
    );
}

#[test]
fn a_coordinator_launch_claims_no_task() {
    // The link is what put it in an agent's row. A coordinator works no task,
    // so the launch queue must not be told it does — otherwise the BOARD points
    // that task at the watcher too, and acting on the task acts on the wrong
    // session.
    assert_eq!(pending_for(Some("Aurora::Quality Assurance")).task_id, None);
}

#[test]
fn an_ordinary_launch_still_claims_its_task() {
    // The other half: this is how every QA session gets linked at all.
    assert_eq!(pending_for(None).task_id, Some(6661));
}

/// What `note_launch` would register for a launch with (or without) a run id.
fn pending_for(run_id: Option<&str>) -> crate::daemon::PendingRequest {
    #[derive(Default)]
    struct Recording(std::sync::Mutex<Vec<crate::daemon::PendingRequest>>);
    impl crate::ui::feed::SessionFeed for Recording {
        fn drain(&mut self) -> Vec<crate::ui::feed::FeedEvent> {
            Vec::new()
        }
        fn request_refresh(&self) {}
        fn note_launch(&self) {}
        fn note_task_launch(&self, request: crate::daemon::PendingRequest) {
            self.0.lock().unwrap().push(request);
        }
        fn set_groups(&self, _group_paths: Vec<String>) {}
    }

    let spec = crate::ui::board::LaunchSpec {
        task_id: 6661,
        cwd: "/repo/alpha".to_string(),
        flags: String::new(),
        prompt: Some("x".to_string()),
        title: "run".to_string(),
        stage_move: None,
        known_session_ids: Vec::new(),
        say: String::new(),
        run_id: run_id.map(str::to_string),
    };
    let feed = Recording::default();
    crate::ui::board::note_launch(&feed, &crate::ui::state::Action::Launch(Box::new(spec)));
    let recorded = feed.0.lock().unwrap();
    recorded.first().cloned().expect("a pending request")
}
