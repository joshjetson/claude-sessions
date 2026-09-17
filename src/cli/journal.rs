//! `claude-sessions journal` — build the reasoning-journal viewer and open it.
//!
//! Ported from the Node app's `bin/journal-viewer.js`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::ConfigHandle;
use crate::db::Db;
use crate::journal::{self, JournalData};
use crate::journal_html;
use crate::paths::Paths;
use crate::term::{Exec, SpawnPolicy};

/// Where the page is written unless `--out` says otherwise.
pub fn default_out(paths: &Paths) -> PathBuf {
    paths.runtime_dir.join("journal.html")
}

pub fn run(
    paths: &Paths,
    config: &ConfigHandle,
    args: super::JournalArgs,
    policy: SpawnPolicy,
) -> Result<()> {
    // The database is one of the two log stores `listLogDates` unions, so the
    // viewer knows about days whose markdown file was moved or deleted.
    let db = Db::open(paths);
    let data = journal::collect(paths, config, Some(&db));

    if args.stats {
        print_stats(&data);
        return Ok(());
    }

    let out = args.out.unwrap_or_else(|| default_out(paths));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::write(&out, journal_html::render_html(&data))
        .with_context(|| format!("could not write {}", out.display()))?;

    println!("{}", journal_html::summary_line(&data));
    println!("{}", out.display());
    if !args.no_open {
        let result = Exec::new(policy).open(&out.display().to_string());
        if !result.ok {
            eprintln!("could not open it: {}", result.failure_message());
        }
    }
    Ok(())
}

fn print_stats(data: &JournalData) {
    println!("repos with docs/agent:  {}", data.repos.len());
    println!("journal entries:        {}", data.entries.len());
    println!("project rules:          {}", data.rules.len());
    println!(
        "archived tasks:         {} ({} with a journal entry)",
        data.tasks.len(),
        data.linked_tasks()
    );
    println!();
    for repo in &data.repos {
        println!(
            "  {:<22} {:>3} entries  {:>3} rules  last {}",
            repo.name,
            repo.entries,
            repo.rules,
            repo.last_entry.as_deref().unwrap_or("—"),
        );
    }
}
