//! `"role"` and the `qa` block's arrival settings: what each value reads as,
//! and what an absent or mistyped one falls back to.

use super::*;
use crate::types::UserRole;

#[test]
fn an_absent_role_is_dev() {
    let (_dir, config) = handle(json!({}));
    assert_eq!(config.role(), UserRole::Dev);
}

#[test]
fn each_role_is_read_and_case_and_space_do_not_matter() {
    for (raw, expected) in [
        ("dev", UserRole::Dev),
        ("qa", UserRole::Qa),
        ("pm", UserRole::Pm),
        (" QA ", UserRole::Qa),
        ("Pm", UserRole::Pm),
    ] {
        let (_dir, config) = handle(json!({ "role": raw }));
        assert_eq!(config.role(), expected, "{raw:?}");
    }
}

/// A typo must not lock someone out of the keys they use every day, and must
/// not fail the parse and lose every other setting in the file.
#[test]
fn an_unknown_or_mistyped_role_is_dev_and_the_file_survives() {
    for raw in [json!("tester"), json!(""), json!(3), json!(null)] {
        let (_dir, config) = handle(json!({ "role": raw, "defaultView": "sessions" }));
        assert_eq!(config.role(), UserRole::Dev, "{raw}");
        assert_eq!(config.default_view(), DefaultView::Sessions, "{raw}");
    }
}

#[test]
fn the_role_round_trips_through_a_save() {
    let (_dir, config) = handle(json!({ "role": "qa" }));
    config.save().unwrap();
    assert_eq!(saved(&config)["role"], json!("qa"));
}

/// The daemon's board and deploy hooks re-read the file through `reloaded()`.
/// The role has to follow an edit the same way.
#[test]
fn an_edited_role_is_seen_by_a_reloaded_handle() {
    let (_dir, config) = handle(json!({ "role": "dev" }));
    fs::write(config.path(), json!({ "role": "qa" }).to_string()).unwrap();
    assert_eq!(
        config.role(),
        UserRole::Dev,
        "the snapshot changed by itself"
    );
    assert_eq!(config.reloaded().role(), UserRole::Qa);
}

#[test]
fn qa_arrival_defaults_match_the_qa_board() {
    let (_dir, config) = handle(json!({}));
    let qa = config.qa_alerts();
    assert_eq!(qa.stages, ["QA", "Quality Assurance", "Tech Debt Work"]);
    assert!(qa
        .revision_stages
        .contains(&"Revision Required".to_string()));
    assert_eq!(qa.projects, None, "no projects mapped means every project");
    assert!(qa.other_qa_user_ids.is_empty());
}

#[test]
fn the_project_default_is_the_mapped_projects() {
    let (_dir, config) = handle(json!({
        "odooProjectDirs": { "Aurora": "~/dev/aurora", "Borealis": ["~/dev/b"] }
    }));
    assert_eq!(
        config.qa_alerts().projects,
        Some(vec!["Aurora".to_string(), "Borealis".to_string()])
    );
}

#[test]
fn an_explicit_project_list_wins_and_an_empty_one_means_every_project() {
    let (_dir, config) = handle(json!({
        "odooProjectDirs": { "Aurora": "~/dev/aurora" },
        "qa": { "projects": ["Borealis"] }
    }));
    assert_eq!(
        config.qa_alerts().projects,
        Some(vec!["Borealis".to_string()])
    );

    let (_dir, config) = handle(json!({
        "odooProjectDirs": { "Aurora": "~/dev/aurora" },
        "qa": { "projects": [] }
    }));
    assert_eq!(config.qa_alerts().projects, None);
}

/// A revision stage is a developer's queue. Listing one by mistake must not
/// turn every sent-back task into a QA arrival.
#[test]
fn configured_stages_replace_the_default_and_drop_revision_stages() {
    let (_dir, config) = handle(json!({
        "qa": { "newTaskStages": ["Ready for QA", "revision required", "  "] }
    }));
    assert_eq!(config.qa_alerts().stages, ["Ready for QA"]);
}

/// Ids are typed by hand, so a quoted one is accepted. Anything else in the
/// list is skipped rather than resetting the lane limit beside it.
#[test]
fn other_qa_ids_forgive_quotes_and_never_cost_the_rest_of_the_block() {
    let (_dir, config) = handle(json!({
        "qa": { "laneLimit": 4, "otherQaUserIds": [24, "25", "a name", 2.5] }
    }));
    assert_eq!(config.qa_alerts().other_qa_user_ids, [24, 25]);
    assert_eq!(config.qa_lane_limit(), Some(4));
}

#[test]
fn board_ignore_carries_into_the_arrival_rule() {
    let (_dir, config) = handle(json!({ "board": { "ignore": ["Old Project"] } }));
    assert_eq!(config.qa_alerts().ignore, ["Old Project"]);
}
