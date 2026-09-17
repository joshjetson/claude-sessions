//! The Node app had no test for `diagnostics.js`. These pin the two things
//! that matter: the watcher stays off unless asked for, and the log cannot grow
//! without bound — a diagnostic that fills a disk is worse than no diagnostic.

use super::*;

#[test]
fn nothing_is_installed_unless_diagnostics_are_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    assert!(start(&paths, "test", false).is_none());
    assert!(!memory_log(&paths).exists());
}

#[test]
fn a_sample_appends_one_named_line() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(tmp.path());
    let log = memory_log(&paths);
    sample(&paths.runtime_dir, &log, "dashboard");

    // Platforms this does not support write nothing rather than a wrong number.
    if memory_reading().is_none() {
        assert!(!log.exists());
        return;
    }
    let body = fs::read_to_string(&log).unwrap();
    assert_eq!(body.lines().count(), 1, "{body}");
    assert!(body.contains(" dashboard "), "{body}");
    assert!(body.contains("MB"), "{body}");
    // The reading is labelled by what it actually is.
    assert!(body.contains("rss=") || body.contains("peak="), "{body}");
}

#[test]
fn the_log_is_trimmed_once_it_passes_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("memory.log");
    let line = format!("{}\n", "x".repeat(200));
    let body: String = std::iter::repeat_n(line, 2000).collect();
    assert!(body.len() as u64 > MAX_LOG_BYTES);
    fs::write(&log, &body).unwrap();

    rotate(&log);
    let trimmed = fs::read_to_string(&log).unwrap();
    assert_eq!(trimmed.lines().count(), KEEP_LINES);
    assert!((trimmed.len() as u64) < MAX_LOG_BYTES);
}

#[test]
fn a_log_under_the_cap_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("memory.log");
    fs::write(&log, "one\ntwo\n").unwrap();
    rotate(&log);
    assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");
}

#[test]
fn a_reading_on_a_supported_platform_is_a_plausible_number() {
    let Some(reading) = memory_reading() else {
        return;
    };
    // Any live process is over zero and well under a terabyte; the point is
    // that the struct layout and unit conversion are not nonsense.
    assert!(reading.megabytes < 1_000_000, "{reading:?}");
}
