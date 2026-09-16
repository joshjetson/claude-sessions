//! Ported from the `protocol` block of `test/daemon.test.js`.
//!
//! The precedence rules are the reason `claude-sessions notify` reaches the
//! daemon at all, and the Node suite pinned them because the helper CLI used to
//! carry its own copy of them.

use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use crate::config::{ConfigHandle, EnvOverrides};
use crate::daemon::protocol::{
    event_frame, percent_decode, read_daemon_info, remove_daemon_info, resolve_port, sse_frame,
    write_daemon_info, DaemonInfo, DEFAULT_PORT,
};
use crate::daemon::EngineEvent;
use crate::paths::Paths;

/// A throwaway tree with a config file, and the environment by value rather
/// than exported — two of these can run at once.
fn setup(config: serde_json::Value, env: EnvOverrides) -> (TempDir, Paths, ConfigHandle) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());
    fs::create_dir_all(&paths.runtime_dir).unwrap();
    fs::write(&paths.config_path, config.to_string()).unwrap();
    let handle = ConfigHandle::load_from(&paths.config_path, &paths.home, env);
    (dir, paths, handle)
}

fn advertise(paths: &Paths, port: u16) {
    fs::write(
        &paths.port_file,
        json!({ "port": port, "pid": 4242, "daemon": true }).to_string(),
    )
    .unwrap();
}

fn env_port(port: u16) -> EnvOverrides {
    EnvOverrides {
        notify_port: Some(port),
        ..EnvOverrides::default()
    }
}

#[test]
fn the_environment_outranks_the_config_which_outranks_the_discovery_file() {
    let (_dir, paths, config) = setup(json!({ "daemon": { "port": 1234 } }), env_port(9999));
    advertise(&paths, 4321);
    assert_eq!(resolve_port(&config, &paths, None), 9999);

    let (_dir, paths, config) = setup(
        json!({ "daemon": { "port": 1234 } }),
        EnvOverrides::default(),
    );
    advertise(&paths, 4321);
    assert_eq!(resolve_port(&config, &paths, None), 1234);

    let (_dir, paths, config) = setup(json!({}), EnvOverrides::default());
    advertise(&paths, 4321);
    assert_eq!(
        resolve_port(&config, &paths, None),
        4321,
        "a running daemon's own file is the last word before the default"
    );

    let (_dir, paths, config) = setup(json!({}), EnvOverrides::default());
    assert_eq!(resolve_port(&config, &paths, None), DEFAULT_PORT);
}

#[test]
fn an_explicit_port_beats_everything_including_the_environment() {
    // `claude-sessions daemon --port` is how a second daemon is run on purpose.
    let (_dir, paths, config) = setup(json!({ "daemon": { "port": 1234 } }), env_port(9999));
    advertise(&paths, 4321);
    assert_eq!(resolve_port(&config, &paths, Some(7777)), 7777);
}

#[test]
fn the_legacy_notify_port_still_names_the_port() {
    let (_dir, paths, config) = setup(json!({ "notifyPort": 5150 }), EnvOverrides::default());
    assert_eq!(resolve_port(&config, &paths, None), 5150);
}

#[test]
fn the_default_port_is_the_one_the_helper_clis_assume() {
    // The Node helpers hardcoded 8787 as their last resort; a drift here would
    // send an agent's notification to a port nothing is listening on.
    assert_eq!(DEFAULT_PORT, 8787);
}

#[test]
fn an_unreadable_discovery_file_is_no_daemon_rather_than_an_error() {
    let (_dir, paths, config) = setup(json!({}), EnvOverrides::default());
    fs::write(&paths.port_file, "{ not json").unwrap();
    assert!(read_daemon_info(&paths).is_none());
    assert_eq!(resolve_port(&config, &paths, None), DEFAULT_PORT);
}

#[test]
fn the_discovery_file_round_trips_and_names_this_process() {
    let (_dir, paths, _config) = setup(json!({}), EnvOverrides::default());
    let written = write_daemon_info(&paths, 8123).unwrap();
    assert_eq!(written.pid, std::process::id());
    assert!(written.daemon);

    let read = read_daemon_info(&paths).expect("the file was not readable back");
    assert_eq!(read, written);
    // camelCase on the wire: `bin/notify.js` and any older daemon read this file.
    let raw: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&paths.port_file).unwrap()).unwrap();
    assert_eq!(raw["port"], 8123);
    assert!(raw["startedAt"].as_str().is_some_and(|ts| !ts.is_empty()));
}

#[test]
fn the_discovery_file_is_removed_only_when_it_still_points_at_us() {
    // A newer daemon may have taken the port over; deleting its file on our way
    // out would leave every helper CLI falling back to the default port.
    let (_dir, paths, _config) = setup(json!({}), EnvOverrides::default());
    write_daemon_info(&paths, 8124).unwrap();

    let someone_else = std::process::id().wrapping_add(1);
    assert!(!remove_daemon_info(&paths, someone_else));
    assert!(
        read_daemon_info(&paths).is_some(),
        "another daemon's file was deleted"
    );

    assert!(remove_daemon_info(&paths, std::process::id()));
    assert!(!Path::new(&paths.port_file).exists());
    // Removing a file that is already gone is not an error either.
    assert!(!remove_daemon_info(&paths, std::process::id()));
}

#[test]
fn a_legacy_discovery_file_without_the_daemon_flag_still_parses() {
    // The in-TUI server omitted `daemon`; its port is still the right port.
    let (_dir, paths, config) = setup(json!({}), EnvOverrides::default());
    fs::write(
        &paths.port_file,
        json!({ "port": 8300, "pid": 1 }).to_string(),
    )
    .unwrap();
    let info: DaemonInfo = read_daemon_info(&paths).unwrap();
    assert!(!info.daemon);
    assert_eq!(resolve_port(&config, &paths, None), 8300);
}

#[test]
fn a_frame_is_the_event_name_then_one_data_line_then_a_blank_line() {
    assert_eq!(sse_frame("sessions", "{}"), "event: sessions\ndata: {}\n\n");
}

#[test]
fn an_engine_event_frames_itself_from_its_own_serde() {
    // The wire format IS the enum's serialisation — there is no second mapping
    // table to keep in step with the engine.
    let frame = event_frame(&EngineEvent::TaskBlocked {
        task_id: 7,
        questions: vec!["which key?".to_string()],
        project: "Project A".to_string(),
    })
    .expect("an event that would not frame");
    assert!(frame.starts_with("event: task-blocked\ndata: {"), "{frame}");
    assert!(frame.ends_with("\n\n"));
    assert!(frame.contains("\"taskId\":7"), "{frame}");
    // The envelope keys never reach the client: the name is the SSE event.
    assert!(!frame.contains("\"event\":"), "{frame}");
}

#[test]
fn percent_escapes_survive_a_project_name() {
    assert_eq!(percent_decode("Project%20A"), "Project A");
    assert_eq!(percent_decode("a%2Fb"), "a/b");
    // A bare percent is a literal, not a truncated escape.
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
}
