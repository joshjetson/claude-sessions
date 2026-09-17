//! What the daemon is built with.
//!
//! The hooks are the daemon's whole connection to Odoo: the board tab of every
//! dashboard talking to it renders what `fetch_board` returned, and the
//! "assigned to you" alert only exists because `fetch_assigned` polls for it.
//! Both were left unwired once, which looked exactly like an empty board.

use super::daemon_options;
use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::{PathEnv, Paths};
use serde_json::json;
use tempfile::TempDir;

/// A throwaway home with the given `~/.claude-sessions.json`.
fn options_for(config: serde_json::Value) -> (TempDir, crate::daemon::EngineOptions) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude-sessions.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let paths = Paths::resolve(dir.path(), &PathEnv::default());
    let handle = ConfigHandle::load_from(&path, dir.path(), EnvOverrides::default());
    let options = daemon_options(paths, handle);
    (dir, options)
}

#[test]
fn complete_odoo_credentials_wire_every_poll_the_daemon_owns() {
    let (_dir, options) = options_for(json!({
        "odoo": {
            "url": "https://odoo.example.com",
            "db": "example",
            "user": "someone@example.com",
            "password": "secret",
        }
    }));
    assert!(options.fetch_board.is_some(), "board poll unwired");
    assert!(
        options.fetch_assigned.is_some(),
        "new-assignment alerts unwired"
    );
    assert!(options.fetch_deploy.is_some(), "deploy poll unwired");
}

#[test]
fn an_install_with_no_credentials_still_builds_an_engine_with_no_polls() {
    let (_dir, options) = options_for(json!({ "groups": [] }));
    assert!(options.fetch_board.is_none());
    assert!(options.fetch_assigned.is_none());
    assert!(options.fetch_deploy.is_none());
}

/// A half-filled block is not credentials. Wiring the polls anyway would make
/// every tick a failed round trip and paint an error over the board.
#[test]
fn partial_credentials_count_as_none() {
    let (_dir, options) = options_for(json!({
        "odoo": { "url": "https://odoo.example.com", "db": "example" }
    }));
    assert!(options.fetch_board.is_none());
    assert!(options.fetch_assigned.is_none());
}

/// The hooks outlive the handle they were built from, so they re-read the file
/// rather than answering from a snapshot: a project filter edited in a dialog
/// has to change what the next poll asks Odoo for.
#[test]
fn the_board_hook_follows_a_config_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude-sessions.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({ "board": { "ignore": ["Aurora"] } })).unwrap(),
    )
    .unwrap();
    let handle = ConfigHandle::load_from(&path, dir.path(), EnvOverrides::default());

    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({ "board": { "ignore": ["Atlas"] } })).unwrap(),
    )
    .unwrap();

    let options =
        crate::odoo::FetchBoardOptions::for_config(&handle.reloaded(), /* mine_only */ true);
    assert_eq!(options.ignore, ["Atlas"]);
    // The handle itself is untouched — only the fresh read sees the edit.
    assert_eq!(handle.board_project_filter().ignore, ["Aurora"]);
}
