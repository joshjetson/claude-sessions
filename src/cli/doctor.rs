//! `claude-sessions doctor` — everything a bug report needs, in one paste.
//!
//! Three developers installed this tool, each had live Claude Code sessions,
//! and each saw an empty dashboard. Nothing on the screen could tell them why,
//! and nothing they could type would say either — so the answer took days and
//! arrived by guesswork. This subcommand is that answer in one screen: what is
//! on the daemon port and which program it belongs to, whether `claude` is a
//! native binary or a script, whether transcripts exist where this build
//! looks, and exactly how many processes survive each step of discovery.
//!
//! Two rules shape it. It states facts rather than verdicts — a number nobody
//! expected is worth more than a green tick. And it prints nothing secret: no
//! password, no token, no transcript content, so the output can be pasted into
//! an issue without being read first.

use std::fmt::Write as _;
use std::fs;
use std::time::SystemTime;

use anyhow::Result;

use crate::config::ConfigHandle;
use crate::daemon::client;
use crate::daemon::protocol;
use crate::paths::Paths;
use crate::scan::{
    argv_is_interactive_claude, is_interactive_claude, is_script_runtime, session_files_in,
    PlatformProcessSource, ProcessSource,
};

mod probe;
#[cfg(test)]
mod tests;

use probe::{ago, env_or, on_path, pid_list, shebang};

pub fn run(paths: &Paths, config: &ConfigHandle) -> Result<()> {
    print!("{}", report(paths, config));
    Ok(())
}

/// The whole report as text.
///
/// Built into a string rather than printed as it goes, so the tests can assert
/// on what a person would paste instead of on the shape of the code that made
/// it.
pub(crate) fn report(paths: &Paths, config: &ConfigHandle) -> String {
    let mut out = String::new();
    versions(&mut out);
    claude_cli(&mut out);
    transcripts(&mut out, paths);
    processes(&mut out);
    daemon(&mut out, paths, config);
    configuration(&mut out, paths, config);
    out
}

fn heading(out: &mut String, title: &str) {
    let _ = writeln!(out, "\n{title}");
}

fn row(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "  {label:<26}{value}");
}

// --- versions ---------------------------------------------------------------

fn versions(out: &mut String) {
    heading(out, "claude-sessions doctor");
    row(
        out,
        "version",
        format!("{} ({})", protocol::VERSION, protocol::IMPLEMENTATION),
    );
    row(
        out,
        "platform",
        format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    if !crate::platform::LIVE_DISCOVERY {
        row(out, "live discovery", "unavailable on this platform");
    }
}

// --- the claude CLI ---------------------------------------------------------

fn claude_cli(out: &mut String) {
    heading(out, "claude CLI");
    let Some(found) = on_path("claude") else {
        row(out, "on PATH", "NOT FOUND");
        row(
            out,
            "",
            "the dashboard can still read transcripts, but nothing will launch",
        );
        return;
    };
    row(out, "on PATH", found.display());
    if let Ok(real) = fs::canonicalize(&found) {
        if real != found {
            row(out, "resolves to", real.display());
        }
    }
    // The distinction that made npm installs invisible: a script is executed
    // by the interpreter on its first line, so `ps` reports the interpreter.
    match shebang(&found) {
        Some(line) => {
            row(out, "kind", format!("script run by `{line}`"));
            row(out, "", "`ps` will show this session as its interpreter");
        }
        None => row(out, "kind", "native binary"),
    }
}

// --- transcripts ------------------------------------------------------------

fn transcripts(out: &mut String, paths: &Paths) {
    heading(out, "transcripts");
    row(out, "store", paths.projects_dir.display());
    row(
        out,
        "CLAUDE_CONFIG_DIR",
        env_or(&["CLAUDE_CONFIG_DIR", "CLAUDE_PROJECTS_DIR"]),
    );
    let Ok(entries) = fs::read_dir(&paths.projects_dir) else {
        row(
            out,
            "state",
            "MISSING — no transcript has ever been written here",
        );
        return;
    };

    let mut dirs = 0usize;
    let mut files = 0usize;
    let mut newest: Option<(SystemTime, String)> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        dirs += 1;
        for file in session_files_in(&dir) {
            files += 1;
            if newest.as_ref().is_none_or(|(at, _)| file.mtime > *at) {
                newest = Some((file.mtime, file.name.clone()));
            }
        }
    }
    row(out, "project directories", dirs);
    row(out, "transcripts", files);
    match newest {
        Some((at, name)) => row(out, "newest", format!("{} ({name})", ago(at))),
        None => row(out, "newest", "none — the store is empty"),
    }
}

// --- discovery --------------------------------------------------------------

fn processes(out: &mut String) {
    heading(out, "processes");
    let source = PlatformProcessSource::default();
    let rows = source.list();
    row(out, "ps rows", rows.len());

    let named: Vec<_> = rows
        .iter()
        .filter(|row| is_interactive_claude(&row.comm))
        .collect();
    row(out, "claude by command name", named.len());

    // The script-install path, and the one extra command-line read it costs.
    let runtimes: Vec<_> = rows
        .iter()
        .filter(|row| is_script_runtime(&row.comm))
        .collect();
    let pids: Vec<u32> = runtimes.iter().map(|row| row.pid).collect();
    let argv = source.argv(&pids);
    let by_argv: Vec<_> = runtimes
        .iter()
        .filter(|row| {
            argv.get(&row.pid)
                .is_some_and(|line| argv_is_interactive_claude(line))
        })
        .collect();
    row(
        out,
        "script runtimes",
        format!(
            "{} (node/bun/deno) → {} are claude",
            runtimes.len(),
            by_argv.len()
        ),
    );
    row(out, "discovery candidates", named.len() + by_argv.len());

    if named.len() + by_argv.len() == 0 && !rows.is_empty() {
        // Nothing passed. What `ps` actually printed is then the only useful
        // thing on the screen, so a few of them go in the report verbatim.
        let sample: Vec<&str> = rows.iter().take(6).map(|row| row.comm.as_str()).collect();
        row(out, "first command names", sample.join(", "));
    }

    // The coexistence check. A second copy of this tool under the same name is
    // what put a dashboard and a daemon of different programs on one port.
    let mine: Vec<u32> = rows
        .iter()
        .filter(|row| row.comm.contains("claude-sessions"))
        .map(|row| row.pid)
        .collect();
    let scripted: Vec<u32> = runtimes
        .iter()
        .filter(|row| {
            argv.get(&row.pid)
                .is_some_and(|line| line.contains("claude-sessions"))
        })
        .map(|row| row.pid)
        .collect();
    row(out, "claude-sessions processes", pid_list(&mine));
    row(
        out,
        "…run by a script runtime",
        match scripted.is_empty() {
            true => "none".to_string(),
            false => format!(
                "{} — the Node tool of the same name is running too",
                pid_list(&scripted)
            ),
        },
    );
}

// --- the daemon -------------------------------------------------------------

fn daemon(out: &mut String, paths: &Paths, config: &ConfigHandle) {
    heading(out, "daemon");
    let port = protocol::resolve_port(config, paths, None);
    row(out, "port", port);
    row(
        out,
        "enabled / autostart",
        format!(
            "{} / {}",
            config.daemon_enabled(),
            config.daemon_autostart()
        ),
    );
    match protocol::read_daemon_info(paths) {
        Some(info) => row(
            out,
            "notify.json",
            format!(
                "port {}, pid {}, daemon={}, started {}",
                info.port, info.pid, info.daemon, info.started_at
            ),
        ),
        None => row(out, "notify.json", "absent or unreadable"),
    }

    match client::probe(port, client::PROBE_TIMEOUT) {
        Some(health) => {
            row(
                out,
                "/health",
                format!(
                    "pid {}, up {}s, {} client(s)",
                    health.pid,
                    health.uptime.round(),
                    health.clients
                ),
            );
            // On its own row, and never abbreviated: this is the line the
            // whole incident turned on.
            row(out, "…is", health.describe());
            if !health.is_this_implementation() {
                row(out, "", "NOT this build — a dashboard will not mirror it");
                row(out, "", "stop it, or give this build its own daemon.port");
            }
        }
        None => row(out, "/health", format!("no answer on :{port}")),
    }

    let log = protocol::daemon_log_path(paths);
    row(out, "daemon.log", log.display());
    match protocol::daemon_log_last_error(paths) {
        Some(error) => row(out, "last error", error),
        None => {
            for line in protocol::daemon_log_tail(paths, 3) {
                row(out, "", line);
            }
        }
    }
}

// --- configuration ----------------------------------------------------------

fn configuration(out: &mut String, paths: &Paths, config: &ConfigHandle) {
    heading(out, "configuration");
    match fs::metadata(&paths.config_path) {
        Ok(meta) => row(
            out,
            "file",
            format!("{} ({} bytes)", paths.config_path.display(), meta.len()),
        ),
        Err(_) => row(
            out,
            "file",
            format!("{} (absent — defaults in use)", paths.config_path.display()),
        ),
    }
    row(out, "groups", config.groups().len());
    // Whether, never what: this output is meant to be pasted somewhere public.
    row(
        out,
        "odoo credentials",
        match config.odoo_creds().is_complete() {
            true => "complete",
            false => "absent or incomplete",
        },
    );
    row(
        out,
        "terminal driver",
        format!("{:?}", config.terminal_driver()).to_lowercase(),
    );
    row(
        out,
        "runtime dir",
        format!(
            "{}{}",
            paths.runtime_dir.display(),
            match paths.is_isolated() {
                true => "  (ISOLATED — not the real dashboard's state)",
                false => "",
            }
        ),
    );
    row(
        out,
        "CLAUDE_SESSIONS_HOME",
        env_or(&["CLAUDE_SESSIONS_HOME"]),
    );
}
