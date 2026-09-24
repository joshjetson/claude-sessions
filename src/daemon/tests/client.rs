//! Ported from the `SSE` block of `test/daemon.test.js`.
//!
//! The reassembly tests are the point of this file. An event can straddle any
//! number of reads, and a naive split-on-blank-line reader drops the tail of
//! every chunk — so the parser is driven here with the nastiest splits that can
//! occur, one byte at a time and through the middle of a multi-byte character,
//! before being exercised against a real socket.

use std::thread;
use std::time::Duration;

use serde_json::json;

use super::server::{collect, data, served, wait_for, PATIENCE};
use super::*;
use crate::daemon::client::{self, SseEvent, SseParser};
use crate::daemon::protocol::{sse_frame, SSE_PING};
use crate::daemon::NewNotification;

/// Feed a whole stream through the parser in fixed-size bites.
fn in_chunks(stream: &str, size: usize) -> Vec<SseEvent> {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for chunk in stream.as_bytes().chunks(size) {
        events.extend(parser.push(chunk));
    }
    events
}

#[test]
fn a_frame_split_across_any_boundary_is_reassembled() {
    let stream = format!(
        "{}{}{}",
        sse_frame("snapshot", &json!({ "a": 1 }).to_string()),
        SSE_PING,
        sse_frame(
            "notification",
            &json!({ "title": "x".repeat(3500) }).to_string()
        ),
    );

    // Every chunk size from one byte up, including sizes that land inside the
    // event name, inside the JSON, and on the blank-line separator itself.
    for size in [1, 2, 3, 7, 13, 64, 1000, 4096, stream.len()] {
        let events = in_chunks(&stream, size);
        assert_eq!(events.len(), 2, "chunk size {size} lost a frame");
        assert_eq!(events[0].event, "snapshot");
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].event, "notification");
        assert_eq!(
            data(&events[1])["title"].as_str().map(str::len),
            Some(3500),
            "chunk size {size} truncated the payload"
        );
    }
}

#[test]
fn a_multi_byte_character_split_across_reads_survives() {
    // A read can land in the middle of a UTF-8 sequence, which is why the
    // parser buffers bytes rather than decoding each chunk as it arrives.
    let stream = sse_frame(
        "notification",
        &json!({ "title": "héllo — ✅" }).to_string(),
    );
    let events = in_chunks(&stream, 1);
    assert_eq!(events.len(), 1);
    assert_eq!(data(&events[0])["title"], json!("héllo — ✅"));
}

#[test]
fn keep_alives_and_dataless_frames_are_skipped() {
    let mut parser = SseParser::new();
    assert!(parser.push(SSE_PING.as_bytes()).is_empty());
    assert!(parser.push(b": another comment\n\n").is_empty());
    assert!(
        parser.push(b"event: lonely\n\n").is_empty(),
        "no data, no event"
    );
    let events = parser.push(b"event: real\ndata: {}\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "real");
}

#[test]
fn multi_line_data_is_joined_and_a_nameless_frame_is_a_message() {
    let mut parser = SseParser::new();
    let events = parser.push(b"data: one\ndata: two\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "message");
    assert_eq!(events[0].data, "one\ntwo");
}

#[test]
fn a_partial_frame_waits_for_the_rest_rather_than_being_reported() {
    let mut parser = SseParser::new();
    assert!(parser.push(b"event: sessions\ndata: {\"by").is_empty());
    assert!(parser.push(b"Project\":{}}").is_empty());
    let events = parser.push(b"\n\n");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\"byProject\":{}}");
}

// --- against a real daemon --------------------------------------------------

#[test]
fn a_subscriber_gets_the_snapshot_first_then_live_events() {
    let served = served();
    let (events, mut subscription) = collect(served.port());
    wait_for("the snapshot", || !events.lock().unwrap().is_empty());
    assert_eq!(
        events.lock().unwrap()[0].event,
        "snapshot",
        "the first frame must be the snapshot, or a client starts blank"
    );

    served
        .engine
        .push_notification(NewNotification::new("test", "streamed", ""));
    wait_for("the live notification", || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.event == "notification" && event.data.contains("streamed"))
    });
    subscription.close();
}

#[test]
fn big_frames_arrive_whole_and_in_order_over_a_real_socket() {
    // Five notifications large enough to be delivered in several TCP segments.
    let served = served();
    let (events, mut subscription) = collect(served.port());
    wait_for("the snapshot", || !events.lock().unwrap().is_empty());

    let big = "x".repeat(3500);
    for n in 0..5 {
        served.engine.push_notification(NewNotification::new(
            "test",
            format!("big-{n}"),
            big.clone(),
        ));
    }

    let titles = || -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.event == "notification")
            .map(|event| {
                data(event)["title"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    };
    wait_for("all five notifications", || titles().len() >= 5);

    assert_eq!(
        titles(),
        (0..5).map(|n| format!("big-{n}")).collect::<Vec<_>>(),
        "frames arrived out of order or were dropped"
    );
    for event in events.lock().unwrap().iter() {
        if event.event == "notification" {
            assert_eq!(
                data(event)["message"].as_str().map(str::len),
                Some(3500),
                "frame payload was truncated"
            );
        }
    }
    subscription.close();
}

#[test]
fn a_closed_subscription_stops_delivering() {
    let served = served();
    let (events, mut subscription) = collect(served.port());
    wait_for("the snapshot", || !events.lock().unwrap().is_empty());

    subscription.close();
    let at_close = events.lock().unwrap().len();
    served
        .engine
        .push_notification(NewNotification::new("test", "after", ""));
    thread::sleep(Duration::from_millis(400));
    assert_eq!(
        events.lock().unwrap().len(),
        at_close,
        "events kept arriving after close()"
    );
}

#[test]
fn a_dropped_subscription_closes_the_stream_too() {
    // The dashboard drops its feed rather than closing it by hand.
    let served = served();
    let (_events, subscription) = collect(served.port());
    wait_for("the client to register", || {
        served.server.client_count() == 1
    });
    drop(subscription);
    wait_for("the client to be forgotten", || {
        served.server.client_count() == 0
    });
}

#[test]
fn the_typed_wrappers_reach_the_routes_they_name() {
    let served = served();
    let client = served.client();
    assert!(client.health().is_some());
    let snapshot = client.state().expect("/state did not parse as a snapshot");
    assert_eq!(snapshot.board_filter, "mine");
    assert!(
        client.refresh_usage(),
        "no usage hook is still an accepted post"
    );
    assert!(client.deploy_log("Project A").is_empty());
    assert!(
        client
            .start_deploy("Project A")
            .is_some_and(|r| r.status == 409),
        "deploys are refused, not unavailable"
    );
    assert!(client.shutdown());
    assert!(served.server.wait_for_shutdown(PATIENCE));
}

#[test]
fn a_probe_of_a_port_with_nothing_on_it_is_none_not_a_hang() {
    // Bind and release, so the port is almost certainly free and unused.
    let listener = crate::daemon::server::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    assert!(client::probe(port, Duration::from_millis(500)).is_none());
}

// --- the dashboard's end of it ----------------------------------------------

#[test]
fn the_dashboard_feed_mirrors_a_daemon_over_the_wire() {
    // The transport the TUI actually runs on: a snapshot becomes the same
    // `Sessions` event a local scan produces, so nothing above the feed knows
    // which end the data came from.
    use crate::ui::feed::{FeedEvent, SessionFeed};
    use crate::ui::feed_remote::RemoteFeed;

    let served = served();
    served.engine.inner().state().sessions =
        index(vec![a_session("s1", "/Users/someone/dev/repo")]);

    let mut feed = RemoteFeed::connect(served.port());
    assert_eq!(feed.port(), served.port());
    let mut seen: Vec<FeedEvent> = Vec::new();

    wait_for("the snapshot to arrive as a sessions event", || {
        seen.extend(feed.drain());
        seen.iter().any(|event| {
            matches!(event, FeedEvent::Sessions { by_project, .. }
                if by_project.contains_key("someone/repo"))
        })
    });

    served
        .engine
        .push_notification(NewNotification::new("test", "from the daemon", ""));
    wait_for("the notification", || {
        seen.extend(feed.drain());
        seen.iter().any(|event| {
            matches!(event, FeedEvent::Notification(notification)
                if notification.title == "from the daemon")
        })
    });

    let before = served.engine.stats().refreshes;
    feed.request_refresh();
    wait_for("the refresh to reach the engine", || {
        served.engine.stats().refreshes > before
    });

    // Shift-Q: the dashboard stops the daemon rather than just detaching.
    feed.shutdown_daemon();
    assert!(served.server.wait_for_shutdown(PATIENCE));
}

#[test]
fn a_complete_empty_tick_reaches_the_dashboard_as_one() {
    // The kill of the last session, seen from a dashboard attached to the
    // daemon: the flag has to survive the snapshot and the mapping into the
    // feed, or the dashboard keeps the dead session on screen.
    use crate::ui::feed::{FeedEvent, SessionFeed};
    use crate::ui::feed_remote::RemoteFeed;

    let served = served();
    served.engine.inner().state().sessions_complete = true;

    let mut feed = RemoteFeed::connect(served.port());
    let mut seen: Vec<FeedEvent> = Vec::new();
    wait_for("the snapshot to arrive as a complete empty list", || {
        seen.extend(feed.drain());
        seen.iter().any(|event| {
            matches!(event, FeedEvent::Sessions { by_project, scan_complete: true, .. }
                if by_project.is_empty())
        })
    });
}

// --- a peer that accepts and says nothing ------------------------------------

/// The v1.0.1 wedge. Something takes the daemon's port and never answers — a
/// daemon that died after `bind`, a half-open socket a sleeping laptop left
/// behind, a stranger on 41777 — and the dashboard's only source of sessions
/// is a reader thread parked in a syscall nobody will end. The screen shows an
/// empty list and no error, for as long as it is left open.
#[test]
fn a_peer_that_accepts_and_never_answers_does_not_hold_the_feed() {
    use crate::test_support::StubDaemon;

    let silent = StubDaemon::silent();
    let (tx, rx) = std::sync::mpsc::channel();
    // The real deadline named explicitly, so the give-up path is exercised
    // without the test waiting it out; the constant itself is pinned below.
    let mut subscription =
        client::subscribe_with(silent.port(), Duration::from_millis(200), move |message| {
            let _ = tx.send(message);
        });

    let first = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the subscriber never gave up on a silent socket");
    assert_eq!(
        first,
        client::SseMessage::Disconnected,
        "a socket that never sent a response head must never be reported connected"
    );
    // …and it keeps trying rather than dying, so the daemon coming up later is
    // still picked up.
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).ok(),
        Some(client::SseMessage::Disconnected)
    );

    // Closing is immediate too: the socket is shut down from this side rather
    // than waited out.
    let closing = std::time::Instant::now();
    subscription.close();
    assert!(
        closing.elapsed() < PATIENCE,
        "closing the subscription hung"
    );
}

#[test]
fn the_handshake_deadline_is_nothing_like_the_streaming_one() {
    // A quiet stream is healthy — the daemon pings every 25 seconds — so the
    // streaming deadline is deliberately long. A silent handshake is never
    // healthy, and sharing one deadline is what made the wedge last a minute
    // at a time. The relationship is the fix; this pins it.
    assert!(
        client::HANDSHAKE_TIMEOUT * 10 < client::READ_TIMEOUT,
        "the handshake deadline has drifted into the streaming one"
    );
    assert!(client::HANDSHAKE_TIMEOUT >= Duration::from_secs(1));
}

// --- who is on the port ------------------------------------------------------

/// The incident this whole change exists for. Two programs ship under the name
/// `claude-sessions` and both bind the same port, so a dashboard of one
/// attached to a daemon of the other: connected, subscribed, and empty
/// forever, with nothing anywhere saying why.
#[test]
fn a_daemon_that_does_not_say_which_program_it_is_is_refused_and_named() {
    use crate::test_support::StubDaemon;

    let stranger = StubDaemon::unmarked();
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());

    // Autostart is on, which makes this the harder case: the port is taken, so
    // starting our own would fail to bind, and attaching is what used to
    // happen instead.
    let refusal = client::ensure_daemon(&paths, stranger.port(), true)
        .expect_err("a daemon with no implementation marker was accepted");
    assert!(
        refusal.contains(&format!("Port {}", stranger.port())),
        "the refusal must name the port: {refusal}"
    );
    assert!(
        refusal.contains("Node tool"),
        "the refusal must name what is probably there: {refusal}"
    );
    assert!(
        refusal.contains("daemon.port"),
        "the refusal must say what to do about it: {refusal}"
    );
}

#[test]
fn a_daemon_of_this_build_says_so_and_is_accepted() {
    let served = served();
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());

    let health = client::probe(served.port(), PATIENCE).expect("no daemon answered");
    assert_eq!(
        health.implementation,
        crate::daemon::protocol::IMPLEMENTATION
    );
    assert_eq!(health.version, env!("CARGO_PKG_VERSION"));
    assert!(health.is_this_implementation());
    assert!(
        health.describe().contains(env!("CARGO_PKG_VERSION")),
        "{}",
        health.describe()
    );

    let target = client::ensure_daemon(&paths, served.port(), false).expect("refused our own");
    assert_eq!(target.port, served.port());
    assert!(!target.spawned);
}

#[test]
fn health_without_a_marker_describes_itself_as_the_older_tool() {
    let legacy: client::Health =
        serde_json::from_str(crate::test_support::UNMARKED_HEALTH).expect("legacy health");
    assert!(legacy.ok);
    assert!(!legacy.is_this_implementation());
    assert!(legacy.describe().contains("older claude-sessions"));
    assert!(legacy.describe().contains("Node tool"));

    // A third implementation names itself, and is still not this one.
    let other: client::Health =
        serde_json::from_str(r#"{"ok":true,"impl":"something-else","version":"9"}"#).unwrap();
    assert!(!other.is_this_implementation());
    assert!(other.describe().contains("something-else"));
}

#[test]
fn a_dead_port_with_autostart_off_explains_itself_rather_than_returning_nothing() {
    // Bind and release, so the port is almost certainly free and unused.
    let listener = crate::daemon::server::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(dir.path());

    let refusal = client::ensure_daemon(&paths, port, false).expect_err("found a daemon");
    assert!(refusal.contains(&format!(":{port}")), "{refusal}");
    assert!(refusal.contains("autostart"), "{refusal}");
}
