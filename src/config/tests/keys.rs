//! Config maps are keyed by Odoo project name, which people type by hand and
//! Odoo renders with its own capitalisation. One lookup rule covers all five
//! maps; the Node app inlined it five times.

use super::*;

#[test]
fn project_keys_are_matched_case_insensitively() {
    let (_dir, config) = handle(json!({
        "targetBranches": { "Storefront": "main" },
        "sshHosts": { "Acme Health": "acme-health-prod" },
        "odooProjectDirs": { "Storefront": ["/tmp/storefront"] },
        "optics": { "projects": { "Charts": "charts" } },
        "deploy": { "projects": { "Storefront": "./ship.sh" } }
    }));
    assert_eq!(config.target_branch("storefront"), Some("main"));
    assert_eq!(config.ssh_host("ACME HEALTH"), Some("acme-health-prod"));
    assert_eq!(
        config.odoo_project_dir_list("STOREFRONT"),
        ["/tmp/storefront"]
    );
    assert_eq!(config.optics_project("charts"), Some("charts"));
    assert_eq!(
        config.deploy_project_config("storefront").unwrap().project,
        "Storefront"
    );
    assert_eq!(config.target_branch("Something Else"), None);
}

#[test]
fn lookup_ci_prefers_an_exact_match_over_a_differently_cased_one() {
    let mut map = BTreeMap::new();
    map.insert("Alpha".to_string(), 1);
    map.insert("ALPHA".to_string(), 2);
    assert_eq!(lookup_ci(&map, "ALPHA"), Some(("ALPHA", &2)));
    assert_eq!(lookup_ci(&map, "Alpha"), Some(("Alpha", &1)));
    // No exact match: the first case-insensitive one wins, deterministically.
    assert_eq!(lookup_ci(&map, "alpha"), Some(("ALPHA", &2)));
    assert_eq!(lookup_ci(&map, "beta"), None);
}
