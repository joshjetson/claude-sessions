//! Drop-in compatibility: a config the tool did not write must come back out of
//! a load/save unchanged, down to the keys this version has never heard of.

use super::*;

#[test]
fn unknown_keys_survive_a_load_save_round_trip() {
    let original = json!({
        "groups": [{ "name": "Dev", "path": "/tmp/dev" }],
        "chat": { "theme": "solarized", "someFutureChatSetting": 7 },
        "aSettingFromANewerVersion": { "nested": ["values", 1, true] },
        "board": { "include": ["One"], "futureBoardKey": "kept" },
        "deploy": { "projects": { "Widgets": { "command": "./ship.sh", "futureKey": 1 } } }
    });
    let (_dir, config) = handle(original);
    config.save().unwrap();
    let after = saved(&config);

    assert_eq!(
        after["aSettingFromANewerVersion"],
        json!({ "nested": ["values", 1, true] })
    );
    assert_eq!(after["chat"]["someFutureChatSetting"], json!(7));
    assert_eq!(after["chat"]["theme"], json!("solarized"));
    assert_eq!(after["board"]["futureBoardKey"], json!("kept"));
    assert_eq!(
        after["deploy"]["projects"]["Widgets"]["futureKey"],
        json!(1)
    );
    assert_eq!(
        after["deploy"]["projects"]["Widgets"]["command"],
        json!("./ship.sh")
    );
}

#[test]
fn a_saved_default_config_looks_like_the_node_one() {
    let (_dir, config) = handle_with(None, EnvOverrides::default());
    config.save().unwrap();
    let raw = fs::read_to_string(config.path()).unwrap();
    assert!(raw.ends_with("}\n"), "config files end with a newline");
    assert!(raw.contains("  \"groups\": []"), "two-space indent: {raw}");
    let after = saved(&config);
    // groups and chat are always written; nothing else is invented.
    assert_eq!(after.as_object().unwrap().len(), 2);
    assert_eq!(after["chat"]["conversationWidth"], json!(25));
}

// --- env precedence ---------------------------------------------------------

#[test]
fn a_fully_populated_config_survives_a_round_trip_key_for_key() {
    // Every block the documented schema has, with a value in each — a save must
    // not quietly drop one. (Empty collections are the one exception: they are
    // written away, because absent and empty mean the same thing everywhere but
    // board.hideStages / board.hideStates, which have their own test.)
    let original = json!({
        "groups": [{ "name": "Dev", "path": "~/dev" }],
        "chat": { "theme": "default", "conversationWidth": 35 },
        "odoo": {
            "url": "https://odoo.example.com",
            "db": "example",
            "user": "you@example.com",
            "password": "REPLACE_ME"
        },
        "odooProjectDirs": { "Your Project": ["~/dev/your-repo"] },
        "targetBranches": { "Your Project": "main" },
        "gitlabHost": "git.example.com",
        "sshHosts": { "Your Project": "your-ssh-alias" },
        "usage": { "enabled": true, "intervalMinutes": 0 },
        "defaultView": "board",
        "notifyPort": 8787,
        "doneStage": ["Quality Assurance", "QA"],
        "inProgressStage": "In Progress",
        "nicknames": { "0198-abcd": "nightly-fix" },
        "board": {
            "include": ["Your Project"],
            "ignore": ["Old Project"],
            "hideStages": ["Deployed"],
            "hideStates": ["1_done", "1_canceled"]
        },
        "deploy": { "projects": { "Your Project": {
            "command": "./scripts/deploy-prod.sh", "cwd": "~/dev/your-repo", "targetBranch": "main"
        } } },
        "optics": {
            "api": "https://optics.example.com",
            "token": "REPLACE_ME",
            "projects": { "Your Project": "your-key" }
        },
        "alerts": {
            "enabled": true,
            "newTaskStages": ["Approved to Start"],
            "stuckAfterMinutes": 15,
            "remindEveryMinutes": 30
        },
        "terminal": { "driver": "auto", "tmuxSession": "claude-sessions" },
        "daemon": { "autostart": true, "port": 8787 }
    });
    let (_dir, config) = handle(original.clone());
    config.save().unwrap();
    let after = saved(&config);

    for (key, value) in original.as_object().unwrap() {
        if key == "chat" {
            // The chat block is the one that grows: the loader fills in every
            // default, exactly as the Node one did, so it is a superset.
            for (setting, value) in value.as_object().unwrap() {
                assert_eq!(&after["chat"][setting], value, "chat.{setting} changed");
            }
            continue;
        }
        assert_eq!(
            after.get(key),
            Some(value),
            "{key} did not survive the round trip"
        );
    }
}

/// One wrongly-typed value used to take the whole file down with it: the parse
/// failed, the loader fell back to `Config::default()`, and an install with
/// working Odoo credentials silently came up with none. Node coerced or guarded
/// per key and only ever lost the file to bad *syntax*.
#[test]
fn a_badly_typed_value_costs_its_own_block_and_nothing_else() {
    let (_dir, config) = handle(json!({
        "odoo": { "url": "https://odoo.example.com", "db": "d", "user": "u", "password": "p" },
        // Every one of these is a shape the Rust types reject and Node tolerated.
        "chat": null,
        "board": { "include": "One Project" },
        "odooProjectDirs": { "One Project": null },
        "deploy": { "projects": { "One Project": null } },
        "nicknames": [],
    }));
    let creds = config.odoo_creds();
    assert!(
        creds.is_complete(),
        "a malformed sibling block discarded the credentials"
    );
    assert_eq!(creds.url, "https://odoo.example.com");
    // The bad blocks fall back to their defaults rather than failing the load.
    assert_eq!(config.chat().theme, "default");
    assert!(config.board_project_filter().include.is_empty());
}

/// A quoted number is a typo, not a reason to drop the block. Node read them
/// all through `Number(x)`.
#[test]
fn a_quoted_minute_count_is_still_a_minute_count() {
    let (_dir, config) = handle(json!({
        "usage": { "enabled": true, "intervalMinutes": "30" },
        "alerts": { "stuckAfterMinutes": "20", "remindEveryMinutes": "0" },
    }));
    assert_eq!(
        config.usage().interval,
        Some(std::time::Duration::from_secs(30 * 60))
    );
    assert_eq!(
        config.alerts().stuck_after,
        std::time::Duration::from_secs(20 * 60)
    );
    assert_eq!(config.alerts().remind_every, std::time::Duration::ZERO);
}

/// The example file ships with the crate, so it is also a promise: every key in
/// it is one this version reads, spelled the way this version spells it. A
/// renamed accessor that forgets the example turns the documented config into a
/// file that silently does nothing.
#[test]
fn the_shipped_example_config_is_read_key_for_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude-sessions.json");
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.json"))
        .expect("config.example.json ships with the crate");
    std::fs::write(&path, &raw).unwrap();
    let config = ConfigHandle::load_from(&path, dir.path(), EnvOverrides::default());

    // Every block, read through the accessor the app itself uses.
    assert_eq!(config.groups().len(), 1);
    assert_eq!(config.default_view(), DefaultView::Board);
    let creds = config.odoo_creds();
    assert!(creds.is_complete());
    assert_eq!(creds.url, "https://odoo.example.com");
    assert_eq!(
        config.odoo_project_dir_list("Your Odoo Project"),
        ["~/dev/your-repo"]
    );
    assert_eq!(config.board_project_filter().include, ["Your Odoo Project"]);
    assert_eq!(config.board_hide_filter().hide_stages, ["Deployed"]);
    assert_eq!(
        config.board_hide_filter().hide_states,
        ["1_done", "1_canceled"]
    );
    assert_eq!(
        config.done_stage(),
        Some(&["Quality Assurance".to_string(), "QA".to_string()][..])
    );
    assert_eq!(
        config.in_progress_stage(),
        Some(&["In Progress".to_string()][..])
    );
    assert_eq!(config.gitlab_host(), Some("git.example.com"));
    assert_eq!(config.target_branch("Your Odoo Project"), Some("main"));
    let deploy = config
        .deploy_project_config("Your Odoo Project")
        .expect("the deploy block");
    assert_eq!(deploy.command, "./scripts/deploy-prod.sh");
    assert_eq!(deploy.target_branch, "main");
    assert_eq!(
        config.ssh_host("Your Odoo Project"),
        Some("your-ssh-host-alias")
    );
    assert_eq!(config.optics_api(), Some("https://optics.example.com"));
    assert_eq!(config.optics_token(), Some("REPLACE_ME"));
    assert_eq!(
        config.optics_project("Your Odoo Project"),
        Some("your-optics-sdk-key")
    );
    assert_eq!(config.chat().conversation_width, 35);
    assert_eq!(config.chat().user_label, "You");
    assert!(config.usage().enabled);
    assert_eq!(config.usage().interval, None);
    let alerts = config.alerts();
    assert!(alerts.enabled);
    assert_eq!(alerts.new_task_stages, ["Approved to Start"]);
    assert_eq!(
        config.sounds().success.as_deref(),
        Some("/System/Library/Sounds/Glass.aiff")
    );
    assert_eq!(config.terminal_driver(), TerminalDriverName::Auto);
    assert_eq!(config.tmux_session(), "claude-sessions");
    assert!(config.daemon_enabled());
    assert!(config.daemon_autostart());
    assert_eq!(config.resolve_port(None, None), 8787);
    assert!(!config.diagnostics());

    // And nothing in it is a key this version would throw away on the next save.
    config.save().unwrap();
    let before: Value = serde_json::from_str(&raw).unwrap();
    let after = saved(&config);
    for (key, value) in before.as_object().unwrap() {
        assert_eq!(
            after.get(key),
            Some(value),
            "the example's `{key}` was lost"
        );
    }
}

/// The example must never carry a real endpoint, credential or company name.
#[test]
fn the_shipped_example_config_names_nothing_real() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.json"))
        .unwrap();
    for host in ["http://", "https://"] {
        for line in raw.lines().filter(|line| line.contains(host)) {
            assert!(
                line.contains("example.com"),
                "an endpoint that is not example.com: {line}"
            );
        }
    }
    assert!(raw.contains("REPLACE_ME"), "secrets must be placeholders");
}
