//! What the report says, and what it must never say.

use std::fs;

use serde_json::json;
use tempfile::TempDir;

use super::probe::shebang;
use super::report;
use crate::config::{ConfigHandle, EnvOverrides};
use crate::paths::Paths;
use crate::test_support::StubDaemon;

/// A throwaway home with the given `~/.claude-sessions.json`.
fn machine(config: serde_json::Value) -> (TempDir, Paths, ConfigHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).expect("runtime dir");
    fs::write(&paths.config_path, config.to_string()).expect("config");
    let handle = ConfigHandle::load_from(&paths.config_path, &paths.home, EnvOverrides::default());
    (dir, paths, handle)
}

#[test]
fn the_report_answers_the_questions_the_incident_needed() {
    let (_dir, paths, config) = machine(json!({ "daemon": { "autostart": false } }));
    fs::create_dir_all(&paths.projects_dir).expect("transcript store");
    let text = report(&paths, &config);

    for expected in [
        // which program this is,
        crate::daemon::protocol::IMPLEMENTATION,
        // where transcripts are looked for,
        "transcripts",
        "store",
        // how many processes survived each step of discovery,
        "ps rows",
        "claude by command name",
        "script runtimes",
        "discovery candidates",
        // whether a second copy of this tool is running,
        "claude-sessions processes",
        // and what is on the port.
        "/health",
        "daemon.log",
    ] {
        assert!(
            text.contains(expected),
            "no `{expected}` in the report:\n{text}"
        );
    }
}

#[test]
fn a_transcript_store_that_does_not_exist_is_said_out_loud() {
    // A first-ever run, and the reason an empty dashboard is correct.
    let (_dir, paths, config) = machine(json!({ "daemon": { "autostart": false } }));
    assert!(!paths.projects_dir.exists());
    let text = report(&paths, &config);
    assert!(text.contains("MISSING"), "{text}");
    assert!(
        text.contains(&paths.projects_dir.display().to_string()),
        "{text}"
    );
}

#[test]
fn a_daemon_that_is_not_this_build_is_named_and_flagged() {
    // The whole incident in one line of output: something else owns the port.
    let stranger = StubDaemon::unmarked();
    let (_dir, paths, config) = machine(json!({
        "daemon": { "port": stranger.port(), "autostart": false }
    }));
    let text = report(&paths, &config);

    assert!(text.contains(&stranger.port().to_string()), "{text}");
    assert!(
        text.contains("NOT this build"),
        "the report did not flag a foreign daemon:\n{text}"
    );
    assert!(text.contains("older claude-sessions"), "{text}");
}

#[test]
fn the_report_is_safe_to_paste_in_public() {
    let (_dir, paths, config) = machine(json!({
        "daemon": { "autostart": false },
        "odoo": {
            "url": "https://odoo.example.com",
            "db": "example",
            "user": "someone@example.com",
            "password": "hunter2-do-not-print-me",
        },
    }));
    let text = report(&paths, &config);

    assert!(
        text.contains("odoo credentials") && text.contains("complete"),
        "{text}"
    );
    for secret in ["hunter2-do-not-print-me", "someone@example.com"] {
        assert!(
            !text.contains(secret),
            "the report leaked `{secret}`:\n{text}"
        );
    }
}

#[test]
fn a_script_install_is_read_off_its_first_line() {
    // The distinction that decides whether `ps` reports `claude` or `node`,
    // and therefore whether a session is visible at all.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("claude");
    fs::write(&script, "#!/usr/bin/env node\nconsole.log('hi')\n").expect("write");
    assert_eq!(shebang(&script).as_deref(), Some("/usr/bin/env node"));

    let binary = dir.path().join("claude-native");
    fs::write(&binary, [0x7f, b'E', b'L', b'F', 0, 0, 0, 0]).expect("write");
    assert_eq!(shebang(&binary), None);
}
