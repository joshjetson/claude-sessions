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
