//! claude-sessions — a live terminal dashboard for Claude Code sessions.
//!
//! The crate is laid out leaf-first: [`types`] holds the shapes, [`paths`] and
//! [`config`] resolve where things live and how they are configured, and
//! everything above them takes those by reference rather than reaching for the
//! environment. `main` builds one [`paths::Paths`] and one
//! [`config::ConfigHandle`] and hands them down.

pub mod archive;
pub mod board;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod db;
pub mod deploy;
pub mod gitlab;
pub mod odoo;
pub mod paths;
pub mod pipeline;
pub mod scan;
pub mod ssh;
pub mod term;
pub mod transcript;
pub mod types;
pub mod ui;
pub mod util;
