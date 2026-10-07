//! `claude-sessions spawn`: start a session for Opus, the way the board would.
//!
//! Opus starts sessions through a `launchClaude(opts)` it imports from a
//! claude-sessions folder. A small Node shim forwards that call here, so a QA
//! session Opus starts gets what a board start gets: the QA pipeline prompt,
//! the round QAden recorded, a reviewer token in the database and the
//! environment, and a pending link the daemon pairs the session with. Without
//! the token the dashboard's approve key refuses the session, and without the
//! pending link the board never shows it.
//!
//! Three kinds:
//!
//! * `qa` — the board's QA launch. Opus's prompt is ignored and the pipeline
//!   prompt is sent instead: the pipeline wraps extra context as "honor this
//!   above generic assumptions", so an Opus template that said "post the note"
//!   would outrank the park rule. It needs the daemon, because pairing and the
//!   approve key both go through it.
//! * `qa-dry` — the developer-facing variant. No token and no duplicate guard,
//!   the same as on the board.
//! * `plain` — Opus's prompt and flags, verbatim. The master's own launch goes
//!   through here, so this path must keep working with no daemon and no task.
//!
//! The answer is one JSON line on stdout, always last. `ok`, `driver`, `hint`
//! and `promptFile` keep the meaning Node gave them, so Opus reads it
//! unchanged. The reviewer token never appears in it, in a log line, or in the
//! prompt file — the prompt names only the environment variable.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::SpawnArgs;
use crate::config::ConfigHandle;
use crate::daemon::{PendingRequest, Snapshot};
use crate::paths::Paths;
use crate::pipeline::definitions::{QA_PRIOR_ROUND_VAR, QA_PRIOR_VERDICT_VAR};
use crate::term::{shell_quote, split_words, DriverKind, SpawnPolicy, TerminalDriver};
use crate::types::Task;
use crate::ui::actions::board::{launch_core, prompt_path, prompt_stem, LaunchCore, LaunchFailure};
use crate::ui::board::launch::{prompt_context, LaunchKind};
use crate::ui::board::start::{qa_extras, reviewer_token, task_url_for};

/// Launched.
pub const EXIT_OK: i32 = 0;
/// The terminal did not open. A token already written stays: the next launch
/// overwrites it, and nothing can sign with it.
pub const EXIT_DRIVER: i32 = 1;
/// The request itself is wrong: no cwd, a missing folder, QA with no task id,
/// a title naming another task.
pub const EXIT_BAD_INPUT: i32 = 2;
/// A live session already works the task.
pub const EXIT_RUNNING: i32 = 3;
/// Spawning is forbidden in this process.
pub const EXIT_FORBIDDEN: i32 = 4;
/// Configuration a QA launch cannot work without: an Odoo URL, or a pipeline
/// the repository names but nothing defines.
pub const EXIT_CONFIG: i32 = 5;
/// No claude-sessions daemon answers, and a QA launch needs one.
pub const EXIT_NO_DAEMON: i32 = 6;

/// One request, as Opus sends it on stdin.
///
/// The first keys mirror `launchClaude(opts)`. `kind`, `round` and `force`
/// are new; `dryRun` is the argv flag's twin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SpawnInput {
    pub kind: Option<String>,
    pub cwd: Option<String>,
    pub task_id: Option<i64>,
    pub title: Option<String>,
    pub round: Option<u32>,
    pub prompt: Option<String>,
    pub claude_flags: Option<String>,
    pub driver: Option<String>,
    pub force: bool,
    pub dry_run: bool,
}

/// Which launch a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    Qa,
    QaDry,
    Plain,
}

impl SpawnKind {
    fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.unwrap_or("plain") {
            "qa" => Some(SpawnKind::Qa),
            "qa-dry" => Some(SpawnKind::QaDry),
            "plain" | "" => Some(SpawnKind::Plain),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SpawnKind::Qa => "qa",
            SpawnKind::QaDry => "qa-dry",
            SpawnKind::Plain => "plain",
        }
    }

    /// The board's kind for the two QA launches; `None` for `plain`.
    fn launch_kind(self) -> Option<LaunchKind> {
        match self {
            SpawnKind::Qa => Some(LaunchKind::Qa),
            SpawnKind::QaDry => Some(LaunchKind::QaDry),
            SpawnKind::Plain => None,
        }
    }
}

/// Everything outside this process that a spawn reads or touches.
///
/// A trait so the tests can answer every question with no daemon, no `ps`,
/// no tmux and no Odoo. [`SystemHost`] is the real one.
pub trait SpawnHost {
    /// The daemon's snapshot, or `None` when no claude-sessions daemon answers.
    fn daemon_state(&self) -> Option<Snapshot>;
    /// `POST /session/pending`. `true` when the daemon accepted it.
    fn post_pending(&self, pending: &PendingRequest) -> bool;
    /// Live sessions on the task that the daemon may not know about: `claude`
    /// processes carrying its task id, and tmux windows named for it. Each is
    /// a short description for the error line.
    fn outside_sessions(&self, task_id: i64) -> Vec<String>;
    /// The driver to launch with: the one asked for, or the configured one.
    fn driver(&self, requested: Option<DriverKind>) -> Option<Arc<dyn TerminalDriver>>;
    /// The task's name, project and stage from Odoo, when it answers.
    fn task(&self, task_id: i64) -> Option<Task>;
    /// The commit a QAden worktree is on. See [`crate::qaden::HeadCache`].
    fn head_of(&self, dir: &Path) -> Option<String>;
}

/// What one spawn decided: the exit code and the JSON line.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub code: i32,
    pub body: Map<String, Value>,
}

impl Outcome {
    pub fn line(&self) -> String {
        Value::Object(self.body.clone()).to_string()
    }
}

/// The fixed inputs of one spawn.
pub struct SpawnContext<'a> {
    pub paths: &'a Paths,
    pub config: &'a ConfigHandle,
    pub policy: SpawnPolicy,
    /// The daemon port, for the error line only.
    pub port: u16,
    pub host: &'a dyn SpawnHost,
}

/// The CLI entry: read the request, spawn, print one line, exit with the code.
pub fn run(
    paths: &Paths,
    config: &ConfigHandle,
    args: SpawnArgs,
    policy: SpawnPolicy,
) -> Result<()> {
    let port = crate::daemon::protocol::resolve_port(config, paths, None);
    let outcome = match read_input(args) {
        Ok(input) => {
            let host = SystemHost {
                config,
                policy,
                port,
            };
            spawn(
                input,
                &SpawnContext {
                    paths,
                    config,
                    policy,
                    port,
                    host: &host,
                },
            )
        }
        Err(error) => refuse(EXIT_BAD_INPUT, error, Map::new()),
    };
    println!("{}", outcome.line());
    if outcome.code != EXIT_OK {
        std::process::exit(outcome.code);
    }
    Ok(())
}

/// The request from stdin, or from the argv flags. On stdin, `--force` and
/// `--dry-run` given on the command line still count.
fn read_input(args: SpawnArgs) -> Result<SpawnInput, String> {
    let mut input = if args.stdin {
        let mut raw = String::new();
        std::io::stdin()
            .read_to_string(&mut raw)
            .map_err(|error| format!("could not read stdin: {error}"))?;
        serde_json::from_str::<SpawnInput>(&raw)
            .map_err(|error| format!("stdin is not a spawn request: {error}"))?
    } else {
        let prompt = match &args.prompt_file {
            Some(file) => Some(
                std::fs::read_to_string(file)
                    .map_err(|error| format!("could not read {}: {error}", file.display()))?,
            ),
            None => None,
        };
        SpawnInput {
            kind: args.kind,
            cwd: args.cwd,
            task_id: args.task,
            title: args.title,
            round: args.round,
            prompt,
            claude_flags: args.flags,
            driver: args.driver,
            ..SpawnInput::default()
        }
    };
    input.force |= args.force;
    input.dry_run |= args.dry_run;
    Ok(input)
}

/// `ok:false`, the error, and whatever was already known.
fn refuse(code: i32, error: impl Into<String>, mut body: Map<String, Value>) -> Outcome {
    body.insert("ok".into(), json!(false));
    body.insert("error".into(), json!(error.into()));
    Outcome { code, body }
}

/// `qa-<task>-r<round>`, the title qa-feed passes, as its two numbers.
fn qa_title(title: &str) -> Option<(i64, u32)> {
    let rest = title.strip_prefix("qa-")?;
    let (task, round) = rest.split_once("-r")?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(task) || !all_digits(round) {
        return None;
    }
    Some((task.parse().ok()?, round.parse().ok()?))
}

/// Windows in a `#{window_name} #{pane_dead}` listing that hold a running
/// session for the task: the board's `task-<id>` or a `qa-<id>-r<round>`.
pub(crate) fn live_task_windows(listing: &str, task_id: i64) -> Vec<String> {
    let board_name = format!("task-{task_id}");
    listing
        .lines()
        .filter_map(|line| line.trim().rsplit_once(' '))
        .filter(|(_, dead)| *dead == "0")
        .map(|(name, _)| name)
        .filter(|name| {
            *name == board_name || qa_title(name).is_some_and(|(named, _)| named == task_id)
        })
        .map(str::to_string)
        .collect()
}

/// The flags a QA launch keeps, re-quoted, and a warning per flag dropped.
///
/// `--worktree` goes in every spelling: QAden makes its own worktree, and a
/// second one from Claude Code would put the session in a folder QAden does
/// not know. `--dangerously-skip-permissions` STAYS when Opus sends it
/// (decision D26): an Opus session waits in a detached tmux pane, and a
/// permission prompt there waits until somebody attaches. The board's own QA
/// launch still sends no bypass. Everything else — `--name`, `--model`,
/// `--settings '<json>'` — is kept word for word, because the Opus hooks module
/// reads its session name from inside that JSON.
pub fn qa_flags(raw: &str) -> Result<(String, Vec<String>), String> {
    let words = split_words(raw).map_err(|error| format!("claudeFlags: {error}"))?;
    let mut kept = Vec::new();
    let mut warnings = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        if word == "--worktree" || word == "-w" {
            match words.next() {
                Some(value) => warnings.push(format!(
                    "dropped {word} {value}: QAden makes its own worktree"
                )),
                None => warnings.push(format!("dropped {word}: QAden makes its own worktree")),
            }
        } else if word.starts_with("--worktree=") {
            warnings.push(format!("dropped {word}: QAden makes its own worktree"));
        } else {
            kept.push(shell_quote(&word));
        }
    }
    Ok((kept.join(" "), warnings))
}

/// The task as the board knows it, then as Odoo knows it, then a bare id.
fn task_for(task_id: i64, snapshot: Option<&Snapshot>, host: &dyn SpawnHost) -> Option<Task> {
    let on_board = snapshot
        .and_then(|snapshot| snapshot.board.as_ref())
        .and_then(|board| {
            board
                .projects
                .values()
                .flat_map(|project| project.stages.values())
                .flat_map(|stage| stage.tasks.iter())
                .find(|task| task.id == task_id)
                .cloned()
        });
    on_board.or_else(|| host.task(task_id))
}

/// Sessions the daemon says work the task: by the transcript's task id, then
/// by the launch link.
fn daemon_sessions(snapshot: &Snapshot, task_id: i64) -> Vec<String> {
    let sessions = snapshot.sessions.by_project.values().flatten();
    let link = snapshot.task_sessions.get(&task_id);
    crate::ui::board::controller::task_session(sessions, task_id, link)
        .map(|session| vec![format!("session {}", session.session_id)])
        .unwrap_or_default()
}

/// One spawn, decided and (unless refused or dry) carried out.
pub fn spawn(input: SpawnInput, cx: &SpawnContext) -> Outcome {
    let mut body = Map::new();
    let mut warnings: Vec<String> = Vec::new();

    // --- the request ---------------------------------------------------------
    let Some(kind) = SpawnKind::parse(input.kind.as_deref()) else {
        return refuse(
            EXIT_BAD_INPUT,
            format!(
                "unknown kind {:?}: use qa, qa-dry or plain",
                input.kind.unwrap_or_default()
            ),
            body,
        );
    };
    body.insert("kind".into(), json!(kind.as_str()));
    let cwd = match input.cwd.as_deref().map(str::trim) {
        None | Some("") => return refuse(EXIT_BAD_INPUT, "cwd is required", body),
        Some(cwd) if !Path::new(cwd).is_dir() => {
            return refuse(EXIT_BAD_INPUT, format!("cwd {cwd} is not a folder"), body)
        }
        Some(cwd) => cwd.to_string(),
    };
    let task_id = input.task_id;
    body.insert("taskId".into(), json!(task_id));
    if kind != SpawnKind::Plain && task_id.is_none() {
        return refuse(
            EXIT_BAD_INPUT,
            format!("kind {} needs a taskId", kind.as_str()),
            body,
        );
    }
    if let (Some(title), Some(task_id)) = (input.title.as_deref(), task_id) {
        if let Some((named, _)) = qa_title(title).filter(|(named, _)| *named != task_id) {
            return refuse(
                EXIT_BAD_INPUT,
                format!("title {title} names task {named}, not taskId {task_id}"),
                body,
            );
        }
    }
    let requested_driver = match input.driver.as_deref() {
        None | Some("") => None,
        Some("tmux") => Some(DriverKind::Tmux),
        Some("iterm2") => Some(DriverKind::Iterm2),
        Some(other) => {
            return refuse(
                EXIT_BAD_INPUT,
                format!("unknown driver {other:?}: use tmux or iterm2"),
                body,
            )
        }
    };

    // --- what the launch needs from outside ----------------------------------
    // A QA launch cannot pair without the task URL: the board finds the
    // session by that URL in the transcript head.
    if kind != SpawnKind::Plain && cx.config.odoo_creds().url.is_empty() {
        return refuse(
            EXIT_CONFIG,
            "no Odoo URL configured: a QA session cannot pair without its task URL",
            body,
        );
    }
    // `plain` with no task asks the daemon nothing, so the master's own launch
    // is as fast with no daemon as with one.
    let snapshot = match (kind, task_id) {
        (SpawnKind::Plain, None) => None,
        _ => cx.host.daemon_state(),
    };
    if snapshot.is_none() {
        match kind {
            // The pending link and the approve key both go through the daemon
            // (decision D28). A QA session started without one is a session
            // nobody can approve.
            SpawnKind::Qa => {
                return refuse(
                    EXIT_NO_DAEMON,
                    format!("no claude-sessions daemon on :{}", cx.port),
                    body,
                )
            }
            SpawnKind::QaDry => warnings.push(format!(
                "no claude-sessions daemon on :{}: the board pairs this session only once a dashboard sees it",
                cx.port
            )),
            SpawnKind::Plain => {}
        }
    }

    // The token is one row per task, and a second spawn overwrites it: the
    // first session could then never be approved from the dashboard. So a
    // live session refuses BEFORE anything is written.
    if let (Some(launch_kind), Some(task_id)) = (kind.launch_kind(), task_id) {
        if launch_kind.guards_duplicates() && !input.force {
            let mut running = snapshot
                .as_ref()
                .map(|snapshot| daemon_sessions(snapshot, task_id))
                .unwrap_or_default();
            running.extend(cx.host.outside_sessions(task_id));
            if !running.is_empty() {
                body.insert("running".into(), json!(running));
                return refuse(
                    EXIT_RUNNING,
                    format!(
                        "a live session already works task {task_id} ({}); pass force to start anyway",
                        running.join(", ")
                    ),
                    body,
                );
            }
        }
    }

    // --- the prompt, the flags, the title ------------------------------------
    let raw_flags = input.claude_flags.clone().unwrap_or_default();
    let (prompt, flags, token, round) = match (kind.launch_kind(), task_id) {
        (Some(launch_kind), Some(task_id)) => {
            let task = task_for(task_id, snapshot.as_ref(), cx.host).unwrap_or_else(|| {
                warnings.push(format!(
                    "task {task_id} not on the board and Odoo did not answer: name, project and stage are blank"
                ));
                Task {
                    id: task_id,
                    ..Task::default()
                }
            });
            let extras = qa_extras(cx.paths, task_id, |dir| cx.host.head_of(dir));
            let prior_round = extras
                .get(QA_PRIOR_ROUND_VAR)
                .and_then(|round| round.parse::<u32>().ok());
            body.insert("priorRound".into(), json!(prior_round));
            body.insert(
                "priorVerdict".into(),
                json!(extras.get(QA_PRIOR_VERDICT_VAR)),
            );
            // A cross-check only: the round is what run.json recorded. A
            // mismatch is said, never refused.
            let expected = prior_round.map_or(1, |round| round + 1);
            if let Some(asked) = input.round.filter(|asked| *asked != expected) {
                warnings.push(format!(
                    "round {asked} asked for, but run.json makes this round {expected}"
                ));
            }
            let context = prompt_context(
                cx.config,
                cx.paths,
                &task,
                task_url_for(cx.config, task_id),
                "",
                &extras,
            );
            let prompt = match context.prompt(&launch_kind, &cwd, None) {
                Ok(prompt) => prompt,
                Err(error) => return refuse(EXIT_CONFIG, error.to_string(), body),
            };
            // Decision D27: the pipeline prompt, whatever Opus sent.
            body.insert("promptIgnored".into(), json!(true));
            let (flags, dropped) = match qa_flags(&raw_flags) {
                Ok(filtered) => filtered,
                Err(error) => return refuse(EXIT_BAD_INPUT, error, body),
            };
            warnings.extend(dropped);
            // A QA session gets one; a dry run does not, the same as the board.
            let token = (kind == SpawnKind::Qa).then(|| reviewer_token(task_id));
            (prompt, flags, token, Some(expected))
        }
        _ => {
            body.insert("promptIgnored".into(), json!(false));
            (input.prompt.clone(), raw_flags, None, input.round)
        }
    };
    body.insert("round".into(), json!(round));

    // Verbatim: Opus's ledger finds the tmux window by this name.
    let title = match (input.title.as_deref().map(str::trim), task_id) {
        (Some(title), _) if !title.is_empty() => title.to_string(),
        (_, Some(task_id)) if kind != SpawnKind::Plain => {
            format!("qa-{task_id}-r{}", round.unwrap_or(1))
        }
        (_, Some(task_id)) => format!("task-{task_id}"),
        (_, None) => "claude".to_string(),
    };
    if let (SpawnKind::Qa, Some(task_id)) = (kind, task_id) {
        if title == format!("task-{task_id}") {
            warnings.push(format!(
                "title {title} is the board's own window name; Opus and the board may each find the other's window"
            ));
        }
    }
    body.insert("title".into(), json!(title));

    let stem = prompt_stem(task_id);
    if input.dry_run {
        body.insert("ok".into(), json!(true));
        body.insert("dryRun".into(), json!(true));
        body.insert(
            "promptFile".into(),
            json!(prompt
                .as_ref()
                .map(|_| prompt_path(cx.paths, &stem).to_string_lossy().into_owned())),
        );
        body.insert("tokenRecorded".into(), json!(false));
        body.insert("pendingPosted".into(), json!(false));
        body.insert("warnings".into(), json!(warnings));
        return Outcome {
            code: EXIT_OK,
            body,
        };
    }

    // --- the launch ----------------------------------------------------------
    let action = match task_id {
        Some(task_id) => format!("launch a session for task {task_id}"),
        None => "launch a session".to_string(),
    };
    if let Err(refused) = cx.policy.check(&action) {
        return refuse(EXIT_FORBIDDEN, refused.message, body);
    }
    let Some(driver) = cx.host.driver(requested_driver) else {
        return refuse(
            EXIT_DRIVER,
            "no terminal driver available: neither tmux nor iTerm2",
            body,
        );
    };

    // Before the terminal opens, the same as the board's `note_launch`: the
    // linker must know which sessions already existed.
    let pending_posted = match (&snapshot, task_id) {
        (Some(snapshot), Some(task_id)) => cx.host.post_pending(&PendingRequest {
            cwd: cwd.clone(),
            task_id: Some(task_id),
            known_session_ids: snapshot
                .sessions
                .by_project
                .values()
                .flatten()
                .map(|session| session.session_id.clone())
                .collect(),
        }),
        _ => false,
    };
    if kind == SpawnKind::Qa && !pending_posted {
        warnings.push(
            "the daemon did not accept the pending link: the board may not pair this session"
                .into(),
        );
    }
    body.insert("pendingPosted".into(), json!(pending_posted));

    let core = LaunchCore {
        task_id,
        cwd: &cwd,
        flags: &flags,
        prompt: prompt.as_deref(),
        title: &title,
        run_id: None,
        reviewer_token: token.as_deref(),
    };
    let result = launch_core(&core, cx.paths, driver.as_ref(), cx.policy);
    body.insert("driver".into(), json!(driver.name()));
    body.insert("tokenRecorded".into(), json!(false));
    body.insert("warnings".into(), json!(warnings));
    match result {
        Ok(launched) => {
            body.insert("ok".into(), json!(true));
            body.insert("hint".into(), json!(launched.hint));
            body.insert("promptFile".into(), json!(launched.prompt_file));
            body.insert("tokenRecorded".into(), json!(token.is_some()));
            Outcome {
                code: EXIT_OK,
                body,
            }
        }
        Err(LaunchFailure::Refused(refused)) => refuse(EXIT_FORBIDDEN, refused.message, body),
        Err(LaunchFailure::Prompt(error)) => refuse(
            EXIT_DRIVER,
            format!("could not write the prompt: {error}"),
            body,
        ),
        Err(LaunchFailure::Driver(reason)) => {
            // The token row was written before the terminal was asked.
            body.insert("tokenRecorded".into(), json!(token.is_some()));
            refuse(
                EXIT_DRIVER,
                format!("could not open a terminal: {reason}"),
                body,
            )
        }
    }
}

// --- the real host -----------------------------------------------------------

/// The daemon on the configured port, `ps`, tmux, and Odoo.
struct SystemHost<'a> {
    config: &'a ConfigHandle,
    policy: SpawnPolicy,
    port: u16,
}

impl SpawnHost for SystemHost<'_> {
    fn daemon_state(&self) -> Option<Snapshot> {
        use crate::daemon::client::{probe, DaemonClient, PROBE_TIMEOUT};
        // The Node daemon answers on the same port and cannot pair a Rust
        // launch, so only this implementation counts.
        probe(self.port, PROBE_TIMEOUT).filter(|info| info.is_this_implementation())?;
        Some(DaemonClient::new(self.port).state().unwrap_or_default())
    }

    fn post_pending(&self, pending: &PendingRequest) -> bool {
        crate::daemon::client::DaemonClient::new(self.port).set_pending(pending)
    }

    fn outside_sessions(&self, task_id: i64) -> Vec<String> {
        use crate::scan::{
            is_interactive_claude, launch_task_id, PlatformProcessSource, ProcessSource,
        };
        let mut found = Vec::new();
        // Only `claude` itself counts. The shell a launch runs in exports the
        // same variable, and an interactive shell left behind after Claude
        // exits is not a session.
        let source = PlatformProcessSource::default();
        let pids: Vec<u32> = source
            .list()
            .into_iter()
            .filter(|row| is_interactive_claude(&row.comm))
            .map(|row| row.pid)
            .collect();
        if !pids.is_empty() {
            let mut hits: Vec<u32> = source
                .environ(&pids)
                .into_iter()
                .filter(|(_, line)| launch_task_id(line) == Some(task_id))
                .map(|(pid, _)| pid)
                .collect();
            hits.sort_unstable();
            found.extend(hits.into_iter().map(|pid| format!("claude pid {pid}")));
        }
        // The session keeps a window open after its command exits (its pane is
        // dead), so only a window whose pane still runs counts.
        let exec = crate::term::Exec::new(self.policy);
        let output = exec.run(
            "tmux",
            &[
                "list-windows".to_string(),
                "-t".to_string(),
                self.config.tmux_session().to_string(),
                "-F".to_string(),
                "#{window_name} #{pane_dead}".to_string(),
            ],
            std::time::Duration::from_secs(3),
        );
        found.extend(
            live_task_windows(&output.stdout, task_id)
                .into_iter()
                .map(|name| format!("tmux window {name}")),
        );
        found
    }

    fn driver(&self, requested: Option<DriverKind>) -> Option<Arc<dyn TerminalDriver>> {
        match requested {
            Some(kind) => Some(crate::term::make_driver(
                kind,
                self.config.tmux_session(),
                self.policy,
            )),
            None => crate::term::get_driver(self.config, self.policy),
        }
    }

    fn task(&self, task_id: i64) -> Option<Task> {
        let creds = self.config.odoo_creds();
        if !creds.is_complete() {
            return None;
        }
        let detail = crate::odoo::OdooClient::new(creds)
            .get_task_detail(task_id)
            .ok()??;
        Some(Task {
            id: detail.id,
            name: detail.name,
            stage_id: detail.stage_id,
            stage_name: detail.stage_name,
            project_id: detail.project_id,
            project_name: detail.project_name,
            ..Task::default()
        })
    }

    fn head_of(&self, dir: &Path) -> Option<String> {
        crate::qaden::HeadCache::new().refresh(&crate::term::Exec::new(self.policy), dir)
    }
}
