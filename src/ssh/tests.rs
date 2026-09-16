//! Ported from the Node app's `test/ssh.test.js` — matching a project to the
//! server it runs on.
//!
//! The fixture below keeps the shape of a real config, quirks included: a
//! space-separated `Host` line, a comma-separated one, a `HostName` with a user
//! glued to the front, and two aliases that share a prefix.

use std::collections::BTreeMap;

use super::*;

const CONFIG: &str = "\
# Edit with caution

Host AppConnects Production
  HostName app.connects.example
  User deploy

Host Edgeport
  HostName ubuntu@portal.edgeport.example

Host planbook,osnode,dentalink
  HostName 192.168.0.194

Host Atlas
  HostName 198.51.100.90

Host atlas-backup
  HostName 198.51.100.90

Host newsdesk
  HostName newsdesk.example.com

Host *
  ServerAliveInterval 60
";

fn hosts() -> Vec<SshHost> {
    parse_ssh_config(CONFIG)
}

fn configured(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn resolve(name: &str) -> SshResolution {
    resolve_ssh_host(name, &hosts(), &BTreeMap::new())
}

fn alias_of(name: &str) -> String {
    match resolve(name) {
        SshResolution::Host(host) => host.alias,
        other => panic!("{name} did not resolve to a host: {other:?}"),
    }
}

// --- parsing ----------------------------------------------------------------

#[test]
fn reads_every_host_block() {
    assert_eq!(hosts().len(), 6);
}

#[test]
fn a_host_line_can_name_several_aliases_space_or_comma_separated() {
    // OpenSSH uses spaces; some tools write commas. A real config has both.
    assert_eq!(hosts()[0].aliases, ["AppConnects", "Production"]);
    assert_eq!(hosts()[2].aliases, ["planbook", "osnode", "dentalink"]);
}

#[test]
fn captures_hostname_user_and_port() {
    let parsed = parse_ssh_config("Host a\n  HostName h.example\n  User deploy\n  Port 2222\n");
    assert_eq!(parsed[0].hostname, "h.example");
    assert_eq!(parsed[0].user, "deploy");
    assert_eq!(parsed[0].port, "2222");
}

#[test]
fn wildcard_blocks_are_skipped_you_do_not_connect_to_host_star() {
    assert!(!hosts()
        .iter()
        .any(|host| host.aliases.iter().any(|a| a == "*")));
}

#[test]
fn directives_after_a_wildcard_block_do_not_leak_onto_the_previous_host() {
    let parsed =
        parse_ssh_config("Host real\n  HostName a.example\n\nHost *\n  HostName b.example\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].hostname, "a.example");
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    assert!(!hosts()
        .iter()
        .any(|host| host.aliases.iter().any(|a| a.starts_with('#'))));
}

#[test]
fn an_empty_config_yields_no_hosts_rather_than_failing() {
    assert!(parse_ssh_config("").is_empty());
    assert!(parse_ssh_config("\n\n   \n").is_empty());
}

#[test]
fn a_missing_file_yields_no_hosts() {
    assert!(load_ssh_hosts(Path::new("/nope/definitely/not/here/config")).is_empty());
}

// --- resolution -------------------------------------------------------------

#[test]
fn matches_a_project_to_its_host_by_name() {
    assert_eq!(alias_of("Atlas"), "Atlas");
    assert_eq!(alias_of("Dentalink"), "dentalink");
    assert_eq!(alias_of("EdgePort"), "Edgeport", "case must not matter");
}

#[test]
fn ignores_punctuation_and_spacing_in_project_names() {
    assert_eq!(alias_of("Pl.an.book"), "planbook");
    assert_eq!(alias_of("App Connects"), "AppConnects");
}

#[test]
fn an_exact_match_beats_a_longer_alias_that_merely_starts_the_same() {
    // "Atlas" must not become "atlas-backup": connecting to the backup server
    // instead of production is exactly the mistake worth preventing.
    match resolve("Atlas") {
        SshResolution::Host(host) => {
            assert_eq!(host.alias, "Atlas");
            assert_eq!(host.source, MatchSource::Exact);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_prefix_match_is_reported_as_one() {
    match resolve("AppConnects Server") {
        SshResolution::Host(host) => {
            assert_eq!(host.alias, "AppConnects");
            assert_eq!(host.source, MatchSource::Prefix);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_match_resolves_to_nothing_so_the_caller_can_open_the_picker() {
    assert_eq!(resolve("Odoo Community"), SshResolution::NoMatch);
    assert_eq!(resolve("Orbit Media"), SshResolution::NoMatch);
}

#[test]
fn an_explicit_config_entry_wins_over_any_guess() {
    let resolved = resolve_ssh_host(
        "Orbit Media",
        &hosts(),
        &configured(&[("Orbit Media", "newsdesk")]),
    );
    match resolved {
        SshResolution::Host(host) => {
            assert_eq!(host.alias, "newsdesk");
            assert_eq!(host.source, MatchSource::Config);
            assert_eq!(host.hostname, "newsdesk.example.com");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_explicit_entry_is_matched_case_insensitively_on_the_project_name() {
    let resolved = resolve_ssh_host(
        "orbit media",
        &hosts(),
        &configured(&[("Orbit Media", "Atlas")]),
    );
    match resolved {
        SshResolution::Host(host) => assert_eq!(host.alias, "Atlas"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_configured_alias_that_is_not_in_the_ssh_config_still_resolves() {
    // The user may keep that host somewhere else; `ssh <alias>` is still the
    // right command to run.
    let resolved = resolve_ssh_host(
        "Anything",
        &hosts(),
        &configured(&[("Anything", "elsewhere")]),
    );
    match resolved {
        SshResolution::Host(host) => {
            assert_eq!(host.alias, "elsewhere");
            assert_eq!(host.hostname, "");
            assert_eq!(host.problem, None);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_ambiguous_prefix_match_is_reported_not_guessed() {
    let two = parse_ssh_config("Host apphost\n HostName a\n\nHost appserver\n HostName b\n");
    match resolve_ssh_host("app", &two, &BTreeMap::new()) {
        SshResolution::Ambiguous(mut candidates) => {
            candidates.sort();
            assert_eq!(candidates, ["apphost", "appserver"]);
        }
        other => panic!("an ambiguous match must not pick one: {other:?}"),
    }
}

#[test]
fn empty_and_punctuation_only_names_resolve_to_nothing() {
    assert_eq!(resolve(""), SshResolution::NoMatch);
    assert_eq!(resolve("!!!"), SshResolution::NoMatch);
}

// --- malformed host blocks --------------------------------------------------

#[test]
fn catches_a_user_embedded_in_hostname() {
    // Real case: `HostName ubuntu@portal.edgeport.example`. ssh tries to resolve
    // the whole string and fails with "could not resolve hostname", which says
    // nothing about the actual mistake.
    let problem = host_config_problem("ubuntu@portal.edgeport.example").expect("a problem");
    assert_eq!(problem.kind, HostProblemKind::UserInHostname);
    assert_eq!(problem.hostname, "portal.edgeport.example");
    assert_eq!(problem.user, "ubuntu");
    assert!(problem.fix.contains("HostName portal.edgeport.example"));
    assert!(problem.fix.contains("User ubuntu"));
}

#[test]
fn a_well_formed_host_has_no_problem() {
    assert_eq!(host_config_problem("portal.edgeport.example"), None);
    assert_eq!(host_config_problem("198.51.100.90"), None);
    assert_eq!(host_config_problem(""), None);
}

#[test]
fn the_problem_travels_with_the_resolved_host() {
    match resolve("EdgePort") {
        SshResolution::Host(host) => {
            let problem = host
                .problem
                .expect("the caller has no way to warn about it");
            assert_eq!(problem.user, "ubuntu");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn resolution_still_succeeds_the_tool_reports_it_does_not_refuse() {
    // The alias is still connectable once the config is fixed, and refusing
    // outright would be worse than connecting and letting ssh say why.
    assert_eq!(alias_of("EdgePort"), "Edgeport");
}

// --- the command ------------------------------------------------------------

#[test]
fn quotes_the_alias_since_aliases_can_contain_spaces() {
    assert_eq!(ssh_command("Atlas"), "ssh 'Atlas'");
    assert_eq!(
        ssh_command("AppConnects Production"),
        "ssh 'AppConnects Production'"
    );
}

#[test]
fn escapes_a_quote_rather_than_breaking_out_of_the_command() {
    assert_eq!(ssh_command("we'ird"), "ssh 'we'\\''ird'");
}

#[test]
fn the_config_path_hangs_off_the_injected_home_not_the_process_environment() {
    assert_eq!(
        ssh_config_path(Path::new("/home/someone")),
        Path::new("/home/someone/.ssh/config")
    );
}

#[test]
fn the_spawn_policy_covers_ssh_too() {
    // Opening a terminal is opening a terminal: the guard that stops a test
    // launching an agent has to stop it connecting to a production server as
    // well. Both routes out of this module — a resolved host and the fallback
    // picker — are shell commands handed to a driver, so there is nothing
    // ssh-specific to guard; this pins that there is no second path.
    use crate::term::{LaunchRequest, SpawnPolicy, TerminalDriver, TmuxDriver};

    let driver = TmuxDriver::new("cs", SpawnPolicy::detect());
    for command in [ssh_command("Atlas"), SSHING_COMMAND.to_string()] {
        let result = driver.launch(&LaunchRequest::new("/tmp", command));
        assert!(!result.ok, "a test opened an ssh session");
        assert!(result.error.unwrap_or_default().contains("Refusing to"));
    }
}
