//! The route table: one arm per endpoint, each a thin forward to an engine
//! action.
//!
//! Split out of the server itself so the transport (sockets, threads, framing)
//! and the contract (paths, status codes, body shapes) can be read separately —
//! the Node original had both in one 315-line file and the routes were the half
//! nobody could find.

use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};

use crate::scan::ProcessSource;
use crate::types::{NotificationKind, NotificationLevel, NotificationStatus};

use crate::daemon::protocol;
use crate::daemon::{
    ActionResult, BlockedMarker, BoardFilter, DoneMarker, NewNotification, PendingRequest,
    RefreshRequest, TaskLinkPatch, TaskLinkStatus,
};

use super::{Shared, SHUTDOWN_DELAY};

pub(super) fn route<S: ProcessSource + Send + 'static>(
    shared: &Arc<Shared<S>>,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Value) {
    let engine = &shared.engine;
    // A malformed body is tolerated rather than fatal: `claude-sessions notify`
    // is invoked by agents from shell one-liners, and a quoting mistake there
    // must still raise a notification rather than 400.
    let body = || -> Value { serde_json::from_slice(body).unwrap_or_else(|_| json!({})) };

    match (method, path) {
        ("GET", "/health") => (
            200,
            json!({
                "ok": true,
                "daemon": true,
                // Which program is on this port, and which build of it. Added
                // fields on an existing contract: an older client ignores
                // them, and a current one refuses to mirror a daemon that
                // does not answer them. See [`protocol::IMPLEMENTATION`].
                "impl": protocol::IMPLEMENTATION,
                "version": protocol::VERSION,
                "pid": std::process::id(),
                "uptime": shared.started.elapsed().as_secs_f64(),
                "clients": shared.clients.len(),
            }),
        ),
        ("GET", "/state") => (
            200,
            serde_json::to_value(engine.snapshot()).unwrap_or(Value::Null),
        ),
        ("POST", "/notify") => {
            let id = engine.push_notification(notification(&body())).id;
            (200, json!({ "ok": true, "id": id }))
        }
        ("POST", "/notifications/status") => {
            let body = body();
            let status = NotificationStatus::from_label(&text(&body, "status", "read"));
            answer(engine.set_notification_status(&id_list(&body, "ids"), status))
        }
        ("POST", "/notifications/dismiss") => {
            answer(engine.dismiss_notifications(&id_list(&body(), "ids")))
        }
        ("POST", "/done") => {
            let body = body();
            engine.process_done(DoneMarker {
                task_id: number(&body, "taskId").unwrap_or_default(),
                cwd: text(&body, "cwd", ""),
                summary: text(&body, "summary", ""),
                ts: crate::util::iso_now(),
            });
            accepted()
        }
        ("POST", "/blocked") => {
            let body = body();
            engine.process_blocked(BlockedMarker {
                task_id: number(&body, "taskId").unwrap_or_default(),
                cwd: text(&body, "cwd", ""),
                questions: id_list(&body, "questions"),
                ts: crate::util::iso_now(),
            });
            accepted()
        }
        ("POST", "/usage/refresh") => {
            // The hook shells out to Claude Code, which takes seconds; the
            // route must not hold a connection open for it.
            let engine = Arc::clone(engine);
            let _ = thread::Builder::new()
                .name("claude-sessions-usage".into())
                .spawn(move || engine.refresh_usage());
            accepted()
        }
        ("POST", "/refresh") => {
            let body = body();
            // The board and the deploy board are fetched by their own workers:
            // the sessions tick does not act on either flag, so honour them
            // here.
            if flag(&body, "board") {
                engine.refresh_board();
            }
            if flag(&body, "deploy") {
                engine.refresh_deploy_board();
            }
            engine.refresh(RefreshRequest {
                force_discovery: flag(&body, "forceDiscovery"),
                board: flag(&body, "board"),
                deploy: flag(&body, "deploy"),
            });
            accepted()
        }
        ("POST", "/board/filter") => {
            // An unrecognised filter leaves the board as it was: this is user
            // input arriving over HTTP.
            if let Some(filter) = BoardFilter::from_label(&text(&body(), "filter", "")) {
                engine.set_board_filter(filter);
            }
            accepted()
        }
        ("POST", "/session/pending") => {
            let body = body();
            engine.set_pending(PendingRequest {
                cwd: text(&body, "cwd", ""),
                task_id: number(&body, "taskId"),
                known_session_ids: id_list(&body, "knownSessionIds"),
            });
            accepted()
        }
        ("POST", "/task/link") => {
            let body = body();
            let info = body.get("info").cloned().unwrap_or_else(|| json!({}));
            engine.link_task(
                number(&body, "taskId").unwrap_or_default(),
                link_patch(&info),
            );
            accepted()
        }
        // 202/409, never 400: "already deploying" and "no command configured"
        // are states the caller can see and fix, not malformed requests.
        ("POST", "/deploy/start") => deployed(engine.start_deploy(&text(&body(), "project", ""))),
        ("POST", "/deploy/cancel") => deployed(engine.cancel_deploy(&text(&body(), "project", ""))),
        ("GET", _) if path.starts_with("/deploy/log/") => {
            // The WHOLE ring buffer, not the trailing window the events carry:
            // this route exists for the times the window is not enough.
            let project = protocol::percent_decode(&path["/deploy/log/".len()..]);
            let lines = engine.deploy_log(&project);
            (200, json!({ "project": project, "lines": lines }))
        }
        ("POST", "/shutdown") => {
            let stop = Arc::clone(&shared.stop);
            let _ = thread::Builder::new()
                .name("claude-sessions-stop".into())
                .spawn(move || {
                    // Long enough for the 200 to reach the client that asked.
                    thread::sleep(SHUTDOWN_DELAY);
                    stop.request();
                });
            (200, json!({ "ok": true }))
        }
        _ => (404, json!({ "ok": false, "error": "not found" })),
    }
}

/// The 202/400 shape the Node routes answered with.
fn answer(result: ActionResult) -> (u16, Value) {
    (
        if result.ok { 202 } else { 400 },
        json!({ "ok": result.ok, "error": result.error }),
    )
}

fn accepted() -> (u16, Value) {
    (202, json!({ "ok": true }))
}

/// A deploy action's answer. A refusal is 409 rather than 400: the request was
/// well formed and the caller can act on the reason.
fn deployed(result: ActionResult) -> (u16, Value) {
    (
        if result.ok { 202 } else { 409 },
        json!({ "ok": result.ok, "error": result.error }),
    )
}

/// The legacy `/notify` contract, unchanged since it was its own server: the
/// title and message are clamped rather than rejected, because the caller is a
/// shell command inside an agent's transcript and a 400 would be invisible.
fn notification(body: &Value) -> NewNotification {
    NewNotification {
        source: "notify",
        // A closed set, resolved through from_label: an unrecognised value
        // reads as Info rather than becoming a new kind. A typo in a --kind
        // flag must not invent one, and must never mark something answerable.
        kind: NotificationKind::from_label(&text(body, "kind", "info")),
        title: clamp(&text(body, "title", "Notification"), 200),
        message: clamp(&text(body, "message", ""), 4000),
        cwd: text(body, "cwd", ""),
        project: body
            .get("project")
            .and_then(Value::as_str)
            .filter(|project| !project.is_empty())
            .map(str::to_string),
        session_id: body
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        task_id: number(body, "taskId"),
        level: NotificationLevel::from_label(&text(body, "level", "info")),
    }
}

fn link_patch(info: &Value) -> TaskLinkPatch {
    TaskLinkPatch {
        cwd: info.get("cwd").and_then(Value::as_str).map(str::to_string),
        session_id: info
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string),
        session_file: info
            .get("sessionFile")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from),
        status: info
            .get("status")
            .and_then(Value::as_str)
            .and_then(|status| match status {
                "running" => Some(TaskLinkStatus::Running),
                "ended" => Some(TaskLinkStatus::Ended),
                "done" => Some(TaskLinkStatus::Done),
                "blocked" => Some(TaskLinkStatus::Blocked),
                _ => None,
            }),
        stage_id: info.get("stageId").and_then(Value::as_i64),
    }
}

/// Clamp by characters, not bytes: slicing UTF-8 mid-character would panic, and
/// a truncated emoji is not what the cap is for.
fn clamp(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        Some((end, _)) => value[..end].to_string(),
        None => value.to_string(),
    }
}

fn text(body: &Value, key: &str, fallback: &str) -> String {
    match body.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => fallback.to_string(),
    }
}

fn number(body: &Value, key: &str) -> Option<i64> {
    match body.get(key) {
        Some(Value::Number(value)) => value.as_i64(),
        // A task id arriving as a string is normal: it came off a command line.
        Some(Value::String(value)) => value.trim().parse().ok(),
        _ => None,
    }
}

fn flag(body: &Value, key: &str) -> bool {
    body.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A list of strings, accepting numbers too — notification ids have been both.
fn id_list(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item {
                    Value::String(value) => Some(value.clone()),
                    Value::Number(value) => Some(value.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
