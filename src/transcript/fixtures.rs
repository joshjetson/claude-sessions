//! The eight scrubbed transcripts, located once.
//!
//! Real sessions with every piece of free text replaced by deterministic filler
//! (Node's `test/tools/make-fixtures.js` did the scrubbing; structure, entry
//! types, tool names, usage numbers and timestamps all survive verbatim). They
//! are the only place the parser meets the real format, so both this module's
//! tests and the status machine's known-gap canaries in `util::tests::status`
//! read them through here.

use std::path::PathBuf;

/// Every fixture transcript, sorted by name.
pub(crate) fn transcripts() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("transcripts");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("fixtures missing at {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());
    paths
}

/// A fixture's file name, for assertion messages.
pub(crate) fn name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into()
}
