//! Deploy entries: the bare-string shorthand, the fallbacks a run resolves
//! through, and writing one back.

use super::*;

#[test]
fn a_bare_string_deploy_entry_is_shorthand_for_a_command() {
    let (_dir, config) = handle(json!({
        "odooProjectDirs": { "Widgets": ["~/dev/widgets"] },
        "deploy": { "projects": { "Widgets": "./scripts/deploy-prod.sh" } }
    }));
    let deploy = config.deploy_project_config("Widgets").unwrap();
    assert_eq!(deploy.project, "Widgets");
    assert_eq!(deploy.command, "./scripts/deploy-prod.sh");
    // cwd falls back to the project's first mapped repo, with ~ expanded.
    assert_eq!(deploy.cwd, Some(config.home.join("dev/widgets")));
    assert_eq!(deploy.target_branch, "main");
}

#[test]
fn a_deploy_object_supplies_its_own_cwd_and_branch() {
    let (_dir, config) = handle(json!({
        "targetBranches": { "widgets": "release" },
        "deploy": { "projects": { "Widgets": {
            "command": "./ship.sh", "cwd": "/srv/widgets", "targetBranch": "production"
        } } }
    }));
    let deploy = config.deploy_project_config("Widgets").unwrap();
    assert_eq!(deploy.cwd, Some(PathBuf::from("/srv/widgets")));
    assert_eq!(deploy.target_branch, "production");
}

#[test]
fn a_deploy_entry_without_a_branch_falls_back_to_the_target_branch_override() {
    let (_dir, config) = handle(json!({
        "targetBranches": { "widgets": "release" },
        "deploy": { "projects": { "Widgets": { "command": "./ship.sh" } } }
    }));
    let deploy = config.deploy_project_config("Widgets").unwrap();
    assert_eq!(deploy.target_branch, "release");
    assert_eq!(deploy.cwd, None);
}

#[test]
fn deploy_entries_can_be_written_and_removed() {
    let (_dir, mut config) = handle(json!({
        "deploy": { "projects": { "Widgets": "./old.sh" } }
    }));
    config
        .set_deploy_project_config(
            "widgets",
            DeployProjectPatch {
                cwd: Some("/srv/widgets".into()),
                ..Default::default()
            },
        )
        .unwrap();
    // The bare string was promoted, the command kept, the entry left under its
    // original key.
    assert_eq!(
        saved(&config)["deploy"]["projects"]["Widgets"],
        json!({ "command": "./old.sh", "cwd": "/srv/widgets" })
    );

    config.remove_deploy_project("Widgets").unwrap();
    assert_eq!(saved(&config)["deploy"]["projects"].get("Widgets"), None);
    assert_eq!(config.deploy_project_names().count(), 0);
}

// --- case-insensitive keys --------------------------------------------------
