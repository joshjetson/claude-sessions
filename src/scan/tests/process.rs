//! Reading `ps` and `lsof` output.

#[cfg(target_os = "macos")]
use crate::scan::is_interactive_claude;
use crate::scan::{parse_lsof_cwd, parse_pid_prefixed, parse_ps_listing, parse_ps_row};
use crate::util::start_time_instant;

const LISTING: &str = "  PID TTY      STARTED                      COMM
    1 ??       Thu Jun  4 09:40:01 2026     /sbin/launchd
 9379 ??       Fri Aug 28 09:24:49 2026     /Users/k/.local/bin/claude
47207 ??       Tue Sep  1 12:51:21 2026     claude bg-pty-host
26085 ttys000  Tue Aug 11 10:45:19 2026     claude
";

#[test]
fn the_header_row_is_not_a_process() {
    let rows = parse_ps_listing(LISTING);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].pid, 1);
}

#[test]
fn a_row_splits_into_pid_tty_start_time_and_command() {
    let row = parse_ps_row(" 26085 ttys000  Tue Aug 11 10:45:19 2026     claude").expect("row");
    assert_eq!(row.pid, 26085);
    assert_eq!(row.tty.as_deref(), Some("ttys000"));
    // Kept verbatim, because the session row renders it through
    // util::format_start_time.
    assert_eq!(row.lstart, "Tue Aug 11 10:45:19 2026");
    assert_eq!(row.comm, "claude");
    assert!(start_time_instant(&row.lstart).is_some());
}

#[test]
fn a_space_padded_day_is_still_a_start_time() {
    // BSD `lstart` pads single-digit days, so the field holds a double space.
    let row =
        parse_ps_row("    1 ??       Thu Jun  4 09:40:01 2026     /sbin/launchd").expect("row");
    assert_eq!(row.lstart, "Thu Jun  4 09:40:01 2026");
    assert_eq!(row.comm, "/sbin/launchd");
    assert!(start_time_instant(&row.lstart).is_some());
}

#[test]
fn no_controlling_terminal_is_none_rather_than_the_string_ps_prints() {
    let row = parse_ps_row(" 9379 ??       Fri Aug 28 09:24:49 2026     /bin/claude").expect("row");
    assert_eq!(row.tty, None);
}

#[test]
fn a_command_containing_four_digits_does_not_eat_the_start_time() {
    // Node's greedy `(.*\d{4})` reached into the command whenever it held four
    // digits before a space.
    let row = parse_ps_row("  501 ttys004  Wed Sep 16 14:10:37 2026     claude --port 8787 x")
        .expect("row");
    assert_eq!(row.lstart, "Wed Sep 16 14:10:37 2026");
    assert_eq!(row.comm, "claude --port 8787 x");
}

#[test]
fn junk_is_skipped_rather_than_guessed_at() {
    assert!(parse_ps_row("").is_none());
    assert!(parse_ps_row("  PID TTY      STARTED    COMM").is_none());
    assert!(parse_ps_row("123").is_none());
    assert!(parse_ps_row("123 ttys000 no year here claude").is_none());
}

#[test]
fn lsof_field_output_yields_the_working_directory() {
    assert_eq!(
        parse_lsof_cwd("p1234\nfcwd\nn/Users/k/dev/app\n").as_deref(),
        Some("/Users/k/dev/app")
    );
    assert_eq!(parse_lsof_cwd("p1234\nfcwd\n"), None);
    assert_eq!(parse_lsof_cwd(""), None);
}

#[test]
fn pid_prefixed_output_keeps_the_rest_of_the_line_verbatim() {
    let map = parse_pid_prefixed(
        "  501 claude --resume abc  \n 502 claude PATH=/usr/bin CLAUDE_SESSIONS_TASK_ID=6137\njunk\n",
    );
    assert_eq!(map.len(), 2);
    assert_eq!(map[&501], "claude --resume abc");
    assert!(map[&502].contains("CLAUDE_SESSIONS_TASK_ID=6137"));
}

/// The one test that touches the real machine: proof that the format above is
/// still what `ps` prints. Ignored by default so the suite stays hermetic.
#[test]
#[ignore = "shells out to the real ps"]
#[cfg(target_os = "macos")]
fn the_real_ps_output_still_parses() {
    use crate::scan::{ProcessSource, SystemProcessSource};

    let rows = SystemProcessSource::new().list();
    assert!(!rows.is_empty(), "ps returned nothing parseable");
    assert!(
        rows.iter().any(|r| start_time_instant(&r.lstart).is_some()),
        "no row carried a readable start time"
    );
    // Whatever is running, `launchd` is pid 1 and is not a claude session.
    assert!(rows.iter().any(|r| r.pid == 1));
    assert!(rows
        .iter()
        .all(|r| !r.comm.is_empty() || !is_interactive_claude(&r.comm)));
}

/// A platform whose process table cannot be read yet answers every question
/// emptily, and never with a panic or a hang — which is what lets the scanner
/// above it run unchanged and simply find nothing live.
#[test]
fn the_unsupported_source_answers_every_call_with_nothing() {
    use crate::scan::{ProcessSource, UnsupportedProcessSource};

    let source = UnsupportedProcessSource::new();
    assert!(source.list().is_empty());
    assert!(source.cwds(&[1, 2, 3]).is_empty());
    assert!(source.argv(&[1, 2, 3]).is_empty());
    assert!(source.environ(&[1, 2, 3]).is_empty());
}

/// And a scanner built on it produces no sessions rather than failing a tick.
#[test]
fn a_scan_on_an_unsupported_platform_is_empty_rather_than_broken() {
    use crate::paths::Paths;
    use crate::scan::{Discovery, Scanner, UnsupportedProcessSource};

    let dir = tempfile::tempdir().expect("tempdir");
    let mut scanner = Scanner::new(
        UnsupportedProcessSource,
        Paths::for_test(dir.path()),
        Discovery::Processes,
    );
    assert!(scanner.processes().is_empty());
    assert!(scanner
        .scan_sessions(std::time::SystemTime::now())
        .is_empty());
}
