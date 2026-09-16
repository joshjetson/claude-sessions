//! Connecting to the server a project runs on.
//!
//! `~/.ssh/config` is the source of truth, because it already is: it is what the
//! user maintains, and what their host picker reads and writes. Duplicating host
//! details into this tool's config would guarantee the two drift apart.
//!
//! A project is matched to a host alias, then a terminal is opened running
//! `ssh <alias>` — so it lands on the right server rather than on a picker. When
//! nothing matches, the picker is launched instead, which is the right tool for
//! "I'm not sure which host".

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::lookup_ci;
use crate::term::shell_quote;
use crate::util::normalise_name;

/// Fallback when no host matches: the user's own interactive picker. It takes no
/// arguments, which is why resolution happens here rather than being delegated.
pub const SSHING_COMMAND: &str = "sshing";

pub fn ssh_config_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("config")
}

/// One `Host` block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshHost {
    pub aliases: Vec<String>,
    pub hostname: String,
    pub user: String,
    pub port: String,
}

/// Parse `~/.ssh/config` into host entries.
///
/// A `Host` line can name several aliases. OpenSSH separates them with spaces;
/// some tools write commas, and a real config has both — so both are handled.
/// Wildcard patterns are skipped: `Host *` is not something you connect to.
pub fn parse_ssh_config(text: &str) -> Vec<SshHost> {
    let mut hosts: Vec<SshHost> = Vec::new();
    let mut in_block = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `Keyword value`: a bare word, then the rest of the line. Anything else
        // (an `=` separator, a continuation) is not something this tool reads.
        let Some(split) = line.find(char::is_whitespace) else {
            continue;
        };
        let (key, value) = line.split_at(split);
        let key = key.to_ascii_lowercase();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        if key == "host" {
            let aliases: Vec<String> = value
                .split([' ', '\t', ','])
                .filter(|alias| !alias.is_empty() && !alias.contains('*') && !alias.contains('?'))
                .map(str::to_string)
                .collect();
            in_block = !aliases.is_empty();
            if in_block {
                hosts.push(SshHost {
                    aliases,
                    ..SshHost::default()
                });
            }
            continue;
        }

        let Some(current) = hosts.last_mut().filter(|_| in_block) else {
            continue;
        };
        match key.as_str() {
            "hostname" => current.hostname = value.to_string(),
            "user" => current.user = value.to_string(),
            "port" => current.port = value.to_string(),
            _ => {}
        }
    }
    hosts
}

/// Read the config, or nothing at all. A machine with no `~/.ssh/config` is
/// normal, and an unreadable one is not worth failing a keystroke over.
pub fn load_ssh_hosts(path: &Path) -> Vec<SshHost> {
    fs::read_to_string(path)
        .map(|text| parse_ssh_config(&text))
        .unwrap_or_default()
}

/// How an alias was arrived at, which is what decides how loudly to say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchSource {
    /// Named in `~/.claude-sessions.json`.
    Config,
    /// The project name and the alias normalise to the same thing.
    Exact,
    /// One starts the other.
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    pub alias: String,
    pub hostname: String,
    pub source: MatchSource,
    /// Something in the host block that will make `ssh <alias>` fail. Carried
    /// rather than acted on: the tool reports, it does not refuse.
    pub problem: Option<HostProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshResolution {
    Host(ResolvedHost),
    /// Several aliases match equally well. Named, never picked between:
    /// connecting to the wrong server is worse than not connecting.
    Ambiguous(Vec<String>),
    /// Nothing matched — the caller opens [`SSHING_COMMAND`].
    NoMatch,
}

/// The ssh alias for a project.
///
/// Explicit config wins. Otherwise an alias is matched on its normalised name —
/// exact first, then a prefix match in either direction, so "LottoEdge" finds
/// "Lottoedge" and "Edentulink" finds the one alias it wants out of a
/// multi-alias `Host` line.
///
/// Exact beats prefix deliberately: "DCA" must not become "dca-backup".
pub fn resolve_ssh_host(
    project_name: &str,
    hosts: &[SshHost],
    configured: &BTreeMap<String, String>,
) -> SshResolution {
    if project_name.is_empty() {
        return SshResolution::NoMatch;
    }

    if let Some((_, alias)) = lookup_ci(configured, project_name) {
        let entry = hosts
            .iter()
            .find(|host| host.aliases.iter().any(|candidate| candidate == alias));
        return SshResolution::Host(ResolvedHost {
            alias: alias.clone(),
            hostname: entry.map(|e| e.hostname.clone()).unwrap_or_default(),
            source: MatchSource::Config,
            problem: entry.and_then(|e| host_config_problem(&e.hostname)),
        });
    }

    let target = normalise_name(project_name);
    if target.is_empty() {
        return SshResolution::NoMatch;
    }

    let flat: Vec<(&str, &SshHost)> = hosts
        .iter()
        .flat_map(|host| host.aliases.iter().map(move |alias| (alias.as_str(), host)))
        .collect();

    let resolved = |(alias, host): &(&str, &SshHost), source| {
        SshResolution::Host(ResolvedHost {
            alias: (*alias).to_string(),
            hostname: host.hostname.clone(),
            source,
            problem: host_config_problem(&host.hostname),
        })
    };

    let exact: Vec<_> = flat
        .iter()
        .filter(|(alias, _)| normalise_name(alias) == target)
        .collect();
    if let [only] = exact[..] {
        return resolved(only, MatchSource::Exact);
    }

    // A project name that starts an alias, or an alias that starts the project
    // name — "LT Connects" vs "LTConnects Production".
    let prefix: Vec<_> = flat
        .iter()
        .filter(|(alias, _)| {
            let alias = normalise_name(alias);
            alias.starts_with(&target) || target.starts_with(&alias)
        })
        .collect();
    match prefix[..] {
        [only] => resolved(only, MatchSource::Prefix),
        [] => SshResolution::NoMatch,
        _ => SshResolution::Ambiguous(
            prefix
                .iter()
                .map(|(alias, _)| (*alias).to_string())
                .collect(),
        ),
    }
}

/// A problem in a host block that will make `ssh <alias>` fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProblem {
    pub kind: HostProblemKind,
    pub message: String,
    /// The lines that would fix it, ready to paste.
    pub fix: String,
    pub hostname: String,
    pub user: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostProblemKind {
    UserInHostname,
}

/// The one seen in practice: `HostName ubuntu@host.example.com`. `HostName`
/// takes a hostname only, so ssh tries to resolve the whole string as DNS and
/// fails with "could not resolve hostname" — which says nothing about the real
/// cause. The user belongs in a `User` directive.
pub fn host_config_problem(hostname: &str) -> Option<HostProblem> {
    let (user, host) = hostname.split_once('@')?;
    let fix = if user.is_empty() {
        format!("HostName {host}")
    } else {
        format!("HostName {host}\n  User {user}")
    };
    Some(HostProblem {
        kind: HostProblemKind::UserInHostname,
        message: format!(
            "HostName is \"{hostname}\" — ssh will try to resolve that whole string as a hostname."
        ),
        fix,
        hostname: host.to_string(),
        user: user.to_string(),
    })
}

/// The command to run in a terminal for a resolved host.
///
/// Quoted because aliases contain spaces — an OpenSSH `Host` line can name
/// "LTConnects Production" and both words are separate aliases, but a configured
/// alias is taken verbatim.
pub fn ssh_command(alias: &str) -> String {
    format!("ssh {}", shell_quote(alias))
}

#[cfg(test)]
mod tests;
