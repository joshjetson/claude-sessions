//! claude-sessions — a live terminal dashboard for Claude Code sessions.
//!
//! The crate is laid out leaf-first: [`types`] holds the shapes, [`paths`] and
//! [`config`] resolve where things live and how they are configured, and
//! everything above them takes those by reference rather than reaching for the
//! environment. `main` builds one [`paths::Paths`] and one
//! [`config::ConfigHandle`] and hands them down.

pub mod archive;
pub mod autodev;
pub mod board;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod dailylog;
pub mod db;
pub mod deploy;
pub mod diagnostics;
pub mod gitlab;
pub mod hook_state;
pub mod http;
pub mod journal;
pub mod journal_html;
pub mod odoo;
pub mod optics;
pub mod paths;
pub mod pipeline;
pub mod platform;
pub mod purge;
pub mod qaden;
pub mod qarun;
pub mod scan;
pub mod ssh;
pub mod term;
pub mod transcript;
pub mod types;
pub mod ui;
pub mod usage;
pub mod util;

#[cfg(test)]
mod test_support;
