//! claude-sessions — a live terminal dashboard for Claude Code sessions.
//!
//! The crate is laid out leaf-first: [`types`] holds the shapes, [`paths`] and
//! [`config`] resolve where things live and how they are configured, and
//! everything above them takes those by reference rather than reaching for the
//! environment. `main` builds one [`paths::Paths`] and one
//! [`config::ConfigHandle`] and hands them down.

pub mod cli;
pub mod config;
pub mod paths;
pub mod transcript;
pub mod types;
pub mod util;
