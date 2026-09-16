//! The write half: every mutator updates the cache and the file together, and
//! leaves everything it was not asked to change alone.

use super::*;
#[test]
fn a_mutation_does_not_disturb_the_rest_of_the_file() {
    let (_dir, mut config) = handle(json!({
        "odoo": { "url": "https://odoo.example.com", "db": "prod" },
        "unknownTopLevel": "kept",
        "nicknames": { "abc-123": "nightly-fix" }
    }));
    config
        .save_session_nickname("def-456", Some("portal"))
        .unwrap();
    let after = saved(&config);
    assert_eq!(after["unknownTopLevel"], json!("kept"));
    assert_eq!(after["odoo"]["url"], json!("https://odoo.example.com"));
    assert_eq!(after["nicknames"]["abc-123"], json!("nightly-fix"));
    assert_eq!(after["nicknames"]["def-456"], json!("portal"));
    assert_eq!(config.session_nickname("def-456"), Some("portal"));
    assert_eq!(config.session_nickname("no-such-session"), None);
    assert_eq!(config.nicknames().len(), 2);

    config.save_session_nickname("abc-123", None).unwrap();
    assert_eq!(saved(&config)["nicknames"].get("abc-123"), None);
}

#[test]
fn the_written_shape_of_a_single_dir_is_preserved() {
    let (_dir, mut config) = handle(json!({ "odooProjectDirs": { "Legacy": "/tmp/legacy" } }));
    config.save().unwrap();
    assert_eq!(
        saved(&config)["odooProjectDirs"]["Legacy"],
        json!("/tmp/legacy")
    );

    // Adding promotes it to a list, which is what the Node writer did.
    config
        .add_odoo_project_dir("Legacy", "/tmp/second")
        .unwrap();
    assert_eq!(
        saved(&config)["odooProjectDirs"]["Legacy"],
        json!(["/tmp/legacy", "/tmp/second"])
    );
    config
        .add_odoo_project_dir("Legacy", "/tmp/second")
        .unwrap();
    assert_eq!(
        config.odoo_project_dir_list("Legacy").len(),
        2,
        "no duplicates"
    );

    config
        .remove_odoo_project_dir("Legacy", "/tmp/legacy")
        .unwrap();
    assert_eq!(
        saved(&config)["odooProjectDirs"]["Legacy"],
        json!(["/tmp/second"])
    );
    config
        .remove_odoo_project_dir("Legacy", "/tmp/second")
        .unwrap();
    assert_eq!(saved(&config)["odooProjectDirs"].get("Legacy"), None);

    // Setting one folder writes the single-string shape the Node setter used.
    config.set_odoo_project_dir("Legacy", "/tmp/only").unwrap();
    assert_eq!(
        saved(&config)["odooProjectDirs"]["Legacy"],
        json!("/tmp/only")
    );
    assert_eq!(config.odoo_project_dir_list("Legacy"), ["/tmp/only"]);
}

// --- deploy entries ---------------------------------------------------------

#[test]
fn setting_a_target_branch_replaces_any_differently_cased_key() {
    let (_dir, mut config) = handle(json!({ "targetBranches": { "storefront": "main" } }));
    config
        .set_target_branch("Storefront", "release/2026")
        .unwrap();
    let after = saved(&config);
    assert_eq!(
        after["targetBranches"],
        json!({ "Storefront": "release/2026" })
    );

    // An empty branch clears it rather than storing a blank.
    config.set_target_branch("Storefront", "  ").unwrap();
    assert_eq!(saved(&config)["targetBranches"].get("Storefront"), None);
}

// --- ~ expansion ------------------------------------------------------------

#[test]
fn a_group_path_is_expanded_when_it_is_written() {
    let (_dir, mut config) = handle(json!({}));
    config.add_group("Dev", "~/dev").unwrap();
    let expected = config.home.join("dev");
    assert_eq!(config.groups()[0].path, expected.to_string_lossy());
    assert_eq!(
        saved(&config)["groups"][0],
        json!({ "name": "Dev", "path": expected.to_string_lossy() })
    );

    // An absolute path is stored as given.
    config.add_group("Other", "/srv/other").unwrap();
    assert_eq!(config.groups()[1].path, "/srv/other");

    config.remove_group(0).unwrap();
    assert_eq!(config.groups().len(), 1);
    assert_eq!(config.groups()[0].name, "Other");
    // Out-of-range removals are a no-op, not a panic.
    config.remove_group(9).unwrap();
    assert_eq!(config.groups().len(), 1);
}

#[test]
fn board_project_filters_round_trip_through_their_setter() {
    let (_dir, mut config) = handle(json!({}));
    config
        .set_board_project_filter(Some(vec!["One".into(), "Two".into()]), None)
        .unwrap();
    assert_eq!(config.board_project_filter().include, ["One", "Two"]);
    assert!(config.board_project_filter().ignore.is_empty());

    config
        .set_board_project_filter(None, Some(vec!["Three".into()]))
        .unwrap();
    // The include half was left alone.
    assert_eq!(config.board_project_filter().include, ["One", "Two"]);
    assert_eq!(saved(&config)["board"]["ignore"], json!(["Three"]));
}

#[test]
fn saving_the_chat_block_replaces_it_wholesale() {
    let (_dir, mut config) = handle(json!({ "chat": { "theme": "monokai" } }));
    let mut chat = config.chat().clone();
    chat.show_timestamps = true;
    chat.conversation_width = 40;
    config.save_chat_config(chat).unwrap();

    assert_eq!(saved(&config)["chat"]["theme"], json!("monokai"));
    assert_eq!(saved(&config)["chat"]["showTimestamps"], json!(true));
    assert_eq!(saved(&config)["chat"]["conversationWidth"], json!(40));
}

#[test]
fn reload_picks_up_a_file_edited_underneath_us() {
    let (_dir, mut config) = handle(json!({ "chat": { "theme": "monokai" } }));
    fs::write(
        config.path(),
        serde_json::to_string(&json!({ "chat": { "theme": "solarized" } })).unwrap(),
    )
    .unwrap();
    // The cache is authoritative until it is told otherwise…
    assert_eq!(config.chat().theme, "monokai");
    config.reload();
    assert_eq!(config.chat().theme, "solarized");
}
