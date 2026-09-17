//! Tests for the dashboard.
//!
//! Split by area, like the modules they cover. [`harness`] holds the two things
//! every file here needs: a `TestBackend` render that returns the painted text,
//! and a throwaway config on a temporary directory.

mod board;
mod conversation;
mod deploy;
mod dialogs;
mod extras_dialogs;
mod harness;
mod keys;
mod layout;
mod render;
mod shutdown;
mod theme;
mod transport;
mod tree;
mod widgets;

pub(crate) use harness::*;
