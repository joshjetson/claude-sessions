//! Config tests run against a real file in a temp directory and an
//! [`EnvOverrides`] literal — never the process environment — so they are safe
//! in parallel and cannot touch the user's own `~/.claude-sessions.json`.

use super::*;
use crate::types::{DefaultView, TerminalDriverName};
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;

/// A handle over a throwaway config file. `home` is the temp directory too, so
/// `~` expansion is assertable.
fn handle_with(contents: Option<Value>, env: EnvOverrides) -> (TempDir, ConfigHandle) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude-sessions.json");
    if let Some(value) = contents {
        fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    }
    let handle = ConfigHandle::load_from(&path, dir.path(), env);
    (dir, handle)
}

fn handle(contents: Value) -> (TempDir, ConfigHandle) {
    handle_with(Some(contents), EnvOverrides::default())
}

/// What is actually on disk after a save.
fn saved(handle: &ConfigHandle) -> Value {
    serde_json::from_str(&fs::read_to_string(handle.path()).unwrap()).unwrap()
}

mod deploy;
mod editing;
mod keys;
mod roles;
mod round_trip;

// --- defaults ---------------------------------------------------------------

#[test]
fn a_missing_file_loads_defaults_rather_than_failing() {
    let (_dir, config) = handle_with(None, EnvOverrides::default());
    assert_eq!(config.chat(), &ChatConfig::default());
    assert!(config.groups().is_empty());
    assert_eq!(config.default_view(), DefaultView::Board);
    assert_eq!(config.gitlab_host(), None);
}

#[test]
fn unreadable_json_loads_defaults_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude-sessions.json");
    fs::write(&path, "{ this is not json").unwrap();
    let config = ConfigHandle::load_from(&path, dir.path(), EnvOverrides::default());
    assert_eq!(config.chat().theme, "default");
}

#[test]
fn chat_defaults_match_the_node_table() {
    let chat = ChatConfig::default();
    assert_eq!(chat.theme, "default");
    assert_eq!(chat.user_color, "cyan");
    assert_eq!(chat.assistant_color, "green");
    assert_eq!(chat.tool_color, "yellow");
    assert_eq!(chat.code_color, "magenta");
    assert_eq!(chat.user_label, "You");
    assert_eq!(chat.assistant_label, "Claude");
    assert!(!chat.show_timestamps);
    assert_eq!(chat.tool_display, "show");
    assert!(!chat.compact_mode);
    assert_eq!(chat.max_lines_per_message, 0);
    assert_eq!(chat.message_filter, "all");
    assert_eq!(chat.search_keyword, "");
    assert_eq!(chat.conversation_width, 25);
    assert!(!chat.swap_panels);
    assert!(chat.show_session_header);
}

#[test]
fn a_partial_chat_block_keeps_the_other_defaults() {
    let (_dir, config) = handle(json!({ "chat": { "theme": "monokai" } }));
    assert_eq!(config.chat().theme, "monokai");
    assert_eq!(config.chat().assistant_label, "Claude");
    assert_eq!(config.chat().conversation_width, 25);
}

#[test]
fn board_and_alert_defaults_apply_when_the_blocks_are_absent() {
    let (_dir, config) = handle(json!({}));
    let hide = config.board_hide_filter();
    assert_eq!(hide.hide_stages, ["Deployed"]);
    assert_eq!(hide.hide_states, ["1_done", "1_canceled"]);

    let alerts = config.alerts();
    assert!(alerts.enabled);
    assert_eq!(alerts.new_task_stages, ["Approved to Start"]);
    assert_eq!(alerts.stuck_after, Duration::from_secs(15 * 60));
    assert_eq!(alerts.remind_every, Duration::from_secs(30 * 60));

    let usage = config.usage();
    assert!(usage.enabled);
    assert_eq!(usage.interval, None);
}

#[test]
fn an_empty_hide_list_means_hide_nothing_not_the_default() {
    // Absent and empty differ here; everywhere else they do not.
    let (_dir, config) = handle(json!({ "board": { "hideStages": [], "hideStates": [] } }));
    assert!(config.board_hide_filter().hide_stages.is_empty());
    assert!(config.board_hide_filter().hide_states.is_empty());
}

#[test]
fn the_legacy_hide_done_state_toggle_is_still_honoured() {
    let (_dir, config) = handle(json!({ "board": { "hideDoneState": false } }));
    assert_eq!(config.board_hide_filter().hide_states, ["1_canceled"]);
}

#[test]
fn alert_reminders_of_zero_mean_once_only() {
    let (_dir, config) = handle(json!({
        "alerts": { "enabled": false, "stuckAfterMinutes": 5, "remindEveryMinutes": 0 }
    }));
    let alerts = config.alerts();
    assert!(!alerts.enabled);
    assert_eq!(alerts.stuck_after, Duration::from_secs(5 * 60));
    assert_eq!(alerts.remind_every, Duration::ZERO);
}

#[test]
fn a_usage_interval_of_zero_stays_manual() {
    let (_dir, config) = handle(json!({ "usage": { "intervalMinutes": 0 } }));
    assert_eq!(config.usage().interval, None);
    let (_dir, config) = handle(json!({ "usage": { "intervalMinutes": 90 } }));
    assert_eq!(config.usage().interval, Some(Duration::from_secs(90 * 60)));
}

// --- clamping ---------------------------------------------------------------

#[test]
fn conversation_width_is_clamped_to_a_usable_range() {
    let width = |value: Value| {
        let (_dir, config) = handle(json!({ "chat": { "conversationWidth": value } }));
        config.chat().conversation_width
    };
    assert_eq!(width(json!(35)), 35);
    assert_eq!(width(json!(3)), 10);
    assert_eq!(width(json!(200)), 80);
    // Zero was falsy in the Node read, so it means "unset".
    assert_eq!(width(json!(0)), 25);
    // A quoted number is a typo, not a reason to reset the whole config.
    assert_eq!(width(json!("40")), 40);
    assert_eq!(width(json!("nonsense")), 25);
}

#[test]
fn odoo_config_beats_the_environment_field_by_field() {
    let env = EnvOverrides {
        odoo_url: Some("https://env.example.com".into()),
        odoo_db: Some("env-db".into()),
        odoo_user: Some("env-user".into()),
        odoo_password: Some("env-password".into()),
        ..Default::default()
    };
    let (_dir, config) = handle_with(
        Some(json!({ "odoo": { "url": "https://config.example.com/", "user": "" } })),
        env,
    );
    let creds = config.odoo_creds();
    // Config wins where it is set…
    assert_eq!(creds.url, "https://config.example.com");
    // …and the env fills the gaps, including a field left empty in config.
    assert_eq!(creds.db, "env-db");
    assert_eq!(creds.user, "env-user");
    assert_eq!(creds.password, "env-password");
    assert!(creds.is_complete());
}

#[test]
fn odoo_creds_are_incomplete_until_every_field_is_present() {
    let (_dir, config) = handle(json!({ "odoo": { "url": "https://odoo.example.com" } }));
    assert!(!config.odoo_creds().is_complete());
}

#[test]
fn gitlab_and_optics_env_override_config() {
    let env = EnvOverrides {
        gitlab_host: Some("git.env.example.com".into()),
        optics_api: Some("https://optics.env.example.com/".into()),
        optics_token: Some("env-token".into()),
        ..Default::default()
    };
    let (_dir, config) = handle_with(
        Some(json!({
            "gitlabHost": "git.config.example.com",
            "optics": { "api": "https://optics.config.example.com", "token": "config-token" }
        })),
        env,
    );
    assert_eq!(config.gitlab_host(), Some("git.env.example.com"));
    assert_eq!(config.optics_api(), Some("https://optics.env.example.com"));
    assert_eq!(config.optics_token(), Some("env-token"));
}

#[test]
fn nothing_is_configured_by_default_for_gitlab_or_optics() {
    // Public build: no company hostname, no bundled token. Unset means off.
    let (_dir, config) = handle(json!({}));
    assert_eq!(config.gitlab_host(), None);
    assert_eq!(config.optics_api(), None);
    assert_eq!(config.optics_token(), None);
    assert_eq!(config.odoo_creds(), crate::types::OdooCreds::default());
}

#[test]
fn port_resolution_follows_the_daemon_order() {
    let configured = json!({ "daemon": { "port": 9001 }, "notifyPort": 9002 });
    let (_dir, config) = handle(configured.clone());
    // daemon.port beats the legacy notifyPort, which beats notify.json.
    assert_eq!(config.resolve_port(None, Some(9003)), 9001);

    let (_dir, config) = handle(json!({ "notifyPort": 9002 }));
    assert_eq!(config.resolve_port(None, Some(9003)), 9002);

    let (_dir, config) = handle(json!({}));
    assert_eq!(config.resolve_port(None, Some(9003)), 9003);
    assert_eq!(config.resolve_port(None, None), DEFAULT_PORT);

    // The env outranks everything configured…
    let env = EnvOverrides {
        notify_port: Some(9100),
        ..Default::default()
    };
    let (_dir, config) = handle_with(Some(configured), env);
    assert_eq!(config.resolve_port(None, Some(9003)), 9100);
    // …but an explicit --port still wins, as it did in the Node daemon.
    assert_eq!(config.resolve_port(Some(9200), Some(9003)), 9200);
}

#[test]
fn a_quoted_port_is_read_rather_than_discarded() {
    let (_dir, config) = handle(json!({ "daemon": { "port": "9001" } }));
    assert_eq!(config.resolve_port(None, None), 9001);
}

// --- string-or-array fields -------------------------------------------------

#[test]
fn done_and_in_progress_stages_accept_a_string_or_a_list() {
    let (_dir, config) = handle(json!({
        "doneStage": "Quality Assurance",
        "inProgressStage": ["In Progress", "Doing"]
    }));
    assert_eq!(
        config.done_stage(),
        Some(&["Quality Assurance".to_string()][..])
    );
    assert_eq!(
        config.in_progress_stage(),
        Some(&["In Progress".to_string(), "Doing".to_string()][..])
    );

    let (_dir, config) = handle(json!({}));
    assert_eq!(config.done_stage(), None);
    assert_eq!(config.in_progress_stage(), None);
}

#[test]
fn a_project_dir_reads_the_same_whether_it_is_a_string_or_a_list() {
    let (_dir, config) = handle(json!({
        "odooProjectDirs": {
            "Legacy": "/tmp/legacy",
            "Modern": ["/tmp/one", "/tmp/two"]
        }
    }));
    assert_eq!(config.odoo_project_dir_list("Legacy"), ["/tmp/legacy"]);
    assert_eq!(
        config.odoo_project_dir_list("Modern"),
        ["/tmp/one", "/tmp/two"]
    );
    assert_eq!(config.odoo_project_dir_list("Absent"), Vec::<String>::new());
    assert_eq!(
        config.odoo_project_names().collect::<Vec<_>>(),
        ["Legacy", "Modern"]
    );
}

#[test]
fn a_hand_written_tilde_path_is_expanded_for_every_reader() {
    // The dialog expands on write, so a group added there is stored absolute.
    // A hand-written one is not, and every consumer compares this path against
    // an absolute working directory — so it is expanded on the way out, and the
    // file keeps the spelling its owner typed.
    let (dir, config) = handle(json!({ "groups": [{ "name": "Dev", "path": "~/dev" }] }));
    let expanded = config.groups()[0].path.clone();
    assert!(!expanded.starts_with('~'), "{expanded}");
    // The rest of the string is kept exactly as written, which is what Node
    // did — so on Windows the expansion reads `C:\Users\k/dev`, one directory
    // spelled two ways. Compared as a directory rather than as bytes for that
    // reason; the tree compares it the same way.
    assert!(
        crate::util::same_dir(&expanded, &dir.path().join("dev").to_string_lossy()),
        "{expanded}"
    );
    config.save().unwrap();
    assert_eq!(saved(&config)["groups"][0]["path"], json!("~/dev"));
}

// --- the rest of the accessors ----------------------------------------------

#[test]
fn view_and_driver_names_fall_back_instead_of_failing_the_parse() {
    let (_dir, config) = handle(json!({
        "defaultView": "kanban",
        "terminal": { "driver": "warp" }
    }));
    assert_eq!(config.default_view(), DefaultView::Board);
    assert_eq!(config.terminal_driver(), TerminalDriverName::Auto);
    // …and the unrecognised values are still in the file afterwards.
    config.save().unwrap();
    assert_eq!(saved(&config)["defaultView"], json!("kanban"));
    assert_eq!(saved(&config)["terminal"]["driver"], json!("warp"));
}

#[test]
fn view_and_driver_names_are_read_when_they_are_recognised() {
    let (_dir, config) = handle(json!({
        "defaultView": "sessions",
        "terminal": { "driver": "tmux", "tmuxSession": "work" },
        "daemon": { "autostart": false, "enabled": false }
    }));
    assert_eq!(config.default_view(), DefaultView::Sessions);
    assert_eq!(config.terminal_driver(), TerminalDriverName::Tmux);
    assert_eq!(config.tmux_session(), "work");
    assert!(!config.daemon_autostart());
    assert!(!config.daemon_enabled());

    let (_dir, config) = handle(json!({}));
    assert_eq!(config.tmux_session(), DEFAULT_TMUX_SESSION);
    assert!(config.daemon_autostart());
}

#[test]
fn a_group_list_of_the_wrong_shape_does_not_lose_the_rest_of_the_config() {
    let (_dir, config) =
        handle(json!({ "groups": { "not": "an array" }, "chat": { "theme": "monokai" } }));
    assert!(config.groups().is_empty());
    assert_eq!(config.chat().theme, "monokai");
}

// --- the coordinator's mode --------------------------------------------------

#[test]
fn a_coordinator_is_in_triage_unless_config_says_shadow() {
    // Triage is the working mode. A coordinator that answers nothing leaves
    // every question with the reviewer, which is the job it exists to take on.
    let (_dir, config) = handle(json!({}));
    assert_eq!(config.qa_coordinator_mode(), crate::qarun::RunMode::Triage);
}

#[test]
fn shadow_is_reachable_from_config() {
    let (_dir, config) = handle(json!({ "qa": { "coordinatorMode": "shadow" } }));
    assert_eq!(config.qa_coordinator_mode(), crate::qarun::RunMode::Shadow);
}

#[test]
fn a_misspelled_mode_falls_back_to_triage_rather_than_silence() {
    // The failure to avoid: "shaddow" resolving to shadow-like behaviour by
    // accident. A coordinator that quietly answers nothing looks exactly like
    // one that is broken, and nobody would know which they had.
    let (_dir, config) = handle(json!({ "qa": { "coordinatorMode": "shaddow" } }));
    assert_eq!(config.qa_coordinator_mode(), crate::qarun::RunMode::Triage);
}

#[test]
fn the_mode_is_case_insensitive() {
    let (_dir, config) = handle(json!({ "qa": { "coordinatorMode": "Shadow" } }));
    assert_eq!(config.qa_coordinator_mode(), crate::qarun::RunMode::Shadow);
}

// --- the automatic lane refill ------------------------------------------------

#[test]
fn lanes_refill_themselves_unless_turned_off() {
    // On by default: the alternative is a reviewer pressing a key every time a
    // session ends, which is why the lane limit was set to uncapped and
    // thirty-five agents ended up resident at once.
    let (_dir, config) = handle(json!({}));
    assert!(config.qa_auto_refill());

    let (_dir, off) = handle(json!({ "qa": { "autoRefill": false } }));
    assert!(!off.qa_auto_refill());

    let (_dir, on) = handle(json!({ "qa": { "autoRefill": true } }));
    assert!(on.qa_auto_refill());
}
