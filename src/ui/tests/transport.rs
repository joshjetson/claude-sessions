//! Which transport is live, whether it is delivering, and what is said when it
//! is not.
//!
//! An empty sessions pane is what this tool looks like on a quiet machine AND
//! what it looks like when its feed is broken. Everything here is about
//! keeping those two apart.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::paths::Paths;
use crate::test_support::StubDaemon;
use crate::ui::feed::{FeedEvent, StaticFeed};
use crate::ui::feed_remote::RemoteFeed;
use crate::ui::transport::{FeedHandle, FeedStatus, Transport};

fn temp_paths() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_test(dir.path());
    std::fs::create_dir_all(&paths.projects_dir).expect("transcript store");
    (dir, paths)
}

fn a_payload() -> FeedEvent {
    FeedEvent::Sessions {
        by_project: BTreeMap::new(),
        discovered: BTreeMap::new(),
        scan_complete: true,
    }
}

#[test]
fn the_status_bar_names_the_transport_and_how_stale_it_is() {
    let now = Instant::now();
    let waiting = FeedStatus {
        transport: Transport::Daemon(41777),
        last_payload: None,
    };
    assert_eq!(waiting.label(now), "daemon :41777 · no data");

    let delivered = FeedStatus {
        transport: Transport::Embedded,
        last_payload: Some(now - Duration::from_secs(3)),
    };
    assert_eq!(delivered.label(now), "embedded · 3s");
}

/// A daemon that connects and then delivers nothing is the shape of every
/// wedge: a stranger on the port, a process stuck after `bind`, a half-open
/// socket. The dashboard stops waiting on it, scans for itself, and says so.
#[test]
fn a_remote_feed_that_delivers_nothing_hands_the_scan_back_and_says_why() {
    let (_dir, paths) = temp_paths();
    let silent = StubDaemon::silent();

    let mut handle =
        FeedHandle::remote(silent.port(), Box::new(RemoteFeed::connect(silent.port())))
            .with_deadline(Duration::from_millis(80));
    assert_eq!(handle.status().transport, Transport::Daemon(silent.port()));
    assert_eq!(handle.status().last_payload, None);

    // Inside the deadline nothing happens: a daemon part-way through its first
    // scan of a busy machine is slow, not broken.
    assert_eq!(handle.fall_back_if_silent(&paths, Vec::new()), None);
    assert_eq!(handle.status().transport, Transport::Daemon(silent.port()));

    std::thread::sleep(Duration::from_millis(120));
    assert!(handle.drain().is_empty(), "the stub delivered something");
    let notice = handle
        .fall_back_if_silent(&paths, Vec::new())
        .expect("the dashboard kept waiting on a feed that never spoke");

    assert_eq!(handle.status().transport, Transport::Embedded);
    assert!(
        notice.contains(&format!(":{}", silent.port())),
        "the notice must name the port: {notice}"
    );
    assert!(
        notice.contains("doctor"),
        "the notice must say where the rest of the answer is: {notice}"
    );

    // And the replacement is a real scan, not an empty stand-in.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut delivered = false;
    while Instant::now() < deadline && !delivered {
        delivered = handle
            .drain()
            .iter()
            .any(|event| matches!(event, FeedEvent::Sessions { .. }));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(delivered, "the embedded scan never produced a payload");
    assert!(handle.status().last_payload.is_some());

    // Once it has taken over it stays taken over.
    assert_eq!(handle.fall_back_if_silent(&paths, Vec::new()), None);
}

#[test]
fn an_empty_payload_is_a_working_daemon_rather_than_a_silent_one() {
    // The distinction the whole mechanism turns on. "No sessions on this
    // machine" is an answer; swapping away from a daemon that gave it would
    // throw away the board, the alerts and the deploys for nothing.
    let (_dir, paths) = temp_paths();
    let mut handle = FeedHandle::remote(41777, Box::new(StaticFeed::with(vec![a_payload()])))
        .with_deadline(Duration::ZERO);

    assert_eq!(handle.drain().len(), 1);
    assert!(handle.status().last_payload.is_some());
    assert_eq!(handle.fall_back_if_silent(&paths, Vec::new()), None);
    assert_eq!(handle.status().transport, Transport::Daemon(41777));
}

#[test]
fn an_embedded_feed_is_never_swapped_out_from_under_itself() {
    // There is nowhere to fall back TO, and a dashboard that replaced its own
    // scanner every five seconds would never finish one.
    let (_dir, paths) = temp_paths();
    let mut handle = FeedHandle::new(Transport::Embedded, Box::new(StaticFeed::default()))
        .with_deadline(Duration::ZERO);
    assert_eq!(handle.fall_back_if_silent(&paths, Vec::new()), None);
    assert_eq!(handle.status().transport, Transport::Embedded);
}

// --- on the screen -----------------------------------------------------------

#[test]
fn the_screen_says_which_transport_it_is_on_and_why_the_list_is_empty() {
    let (_dir, mut state) = crate::ui::tests::sessions_state();
    state.feed = FeedStatus {
        transport: Transport::Daemon(41777),
        last_payload: None,
    };
    state.note_feed("Port 41777 is held by something else.");
    state.transcripts_notice = Some("No transcripts under /tmp/none.".to_string());

    let painted = crate::ui::tests::text(&crate::ui::tests::render(100, 30, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));

    // The status bar: which end the sessions came from, and how long ago.
    assert!(
        painted.contains("daemon :41777"),
        "the status bar never named the transport:\n{painted}"
    );
    assert!(
        painted.contains("no data"),
        "the status bar never said the feed has delivered nothing:\n{painted}"
    );
    // The pane somebody is actually staring at.
    assert!(painted.contains("No active sessions found."), "{painted}");
    assert!(
        painted.contains("held by something else"),
        "the empty pane never explained itself:\n{painted}"
    );
    assert!(painted.contains("No transcripts under"), "{painted}");
}

#[test]
fn a_working_feed_says_so_without_explaining_anything() {
    let (_dir, mut state) = crate::ui::tests::sessions_state();
    state.feed = FeedStatus {
        transport: Transport::Embedded,
        last_payload: Some(Instant::now()),
    };

    let painted = crate::ui::tests::text(&crate::ui::tests::render(100, 30, |frame| {
        crate::ui::app::draw(frame, &mut state)
    }));
    assert!(painted.contains("embedded · 0s"), "{painted}");
    // Nothing to explain: an empty list on a quiet machine is just an empty
    // list, and a notice that is always there is a notice nobody reads.
    assert!(painted.contains("No active sessions found."), "{painted}");
    assert!(!painted.contains("doctor"), "{painted}");
}
