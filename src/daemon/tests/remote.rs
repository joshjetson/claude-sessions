//! The dashboard's side of the wire: what a [`RemoteFeed`] does with the
//! snapshot a daemon hands it on connect.
//!
//! A daemon warms its board without awaiting it, so a dashboard that connects
//! during the warm-up is handed a snapshot with no board in it. Nothing ever
//! filled that in: the board tab stayed empty until somebody pressed `r`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::server::{served_with, wait_for};
use crate::daemon::Snapshot;
use crate::types::{Board, DeployBoard};
use crate::ui::feed::SessionFeed;
use crate::ui::feed_remote::{Kicks, RemoteFeed};

/// A fetch that counts its calls and answers with an empty board.
fn counting() -> (Arc<AtomicUsize>, crate::daemon::BoardFetch) {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = Arc::clone(&calls);
    let fetch: crate::daemon::BoardFetch = Box::new(move |_filter| {
        handle.fetch_add(1, Ordering::SeqCst);
        Ok(Board::default())
    });
    (calls, fetch)
}

#[test]
fn connecting_to_a_daemon_with_no_board_yet_asks_for_one_exactly_once() {
    let (board_calls, board) = counting();
    let deploy_calls = Arc::new(AtomicUsize::new(0));
    let deploy_handle = Arc::clone(&deploy_calls);
    let deploy: crate::daemon::DeployFetch = Box::new(move || {
        deploy_handle.fetch_add(1, Ordering::SeqCst);
        Ok(DeployBoard::default())
    });

    // Never started, which is a daemon whose warm-up has not landed: the
    // snapshot a client gets carries no board and no deploy list.
    let served = served_with(Some(board), Some(deploy));
    assert_eq!(board_calls.load(Ordering::SeqCst), 0);

    let mut feed = RemoteFeed::connect(served.port());
    wait_for("the board to be fetched", || {
        board_calls.load(Ordering::SeqCst) == 1
    });
    wait_for("the deploy list to be fetched", || {
        deploy_calls.load(Ordering::SeqCst) == 1
    });

    // …and only once. The engine's own pollers are not running here, so any
    // further call could only come from the client asking again.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(board_calls.load(Ordering::SeqCst), 1, "asked twice");
    assert_eq!(deploy_calls.load(Ordering::SeqCst), 1, "asked twice");

    // The board that arrives reaches the dashboard as an update it can apply.
    wait_for("the board to reach the feed", || {
        feed.drain()
            .iter()
            .any(|event| matches!(event, crate::ui::feed::FeedEvent::Board(_)))
    });
}

#[test]
fn a_reconnect_does_not_ask_again() {
    // The subscription retries on a backoff and every reconnect delivers a
    // fresh snapshot; claiming the nudge is what stops that becoming a poll.
    let kicks = Kicks::default();
    let empty = Snapshot::default();
    assert_eq!(kicks.claim(&empty), (true, true), "the first snapshot asks");
    assert_eq!(
        kicks.claim(&empty),
        (false, false),
        "every snapshot after it stays quiet"
    );
}

#[test]
fn a_daemon_that_already_has_a_board_is_left_alone() {
    let kicks = Kicks::default();
    let loaded = Snapshot {
        board: Some(Board::default()),
        deploy: Some(DeployBoard::default()),
        ..Snapshot::default()
    };
    assert_eq!(kicks.claim(&loaded), (false, false));
}

#[test]
fn a_fetch_already_in_flight_is_not_asked_for_twice() {
    // `boardLoading` is the daemon saying it is on its way; the settled event
    // will carry it.
    let kicks = Kicks::default();
    let warming = Snapshot {
        board_loading: true,
        deploy: Some(DeployBoard::default()),
        ..Snapshot::default()
    };
    assert_eq!(kicks.claim(&warming), (false, false));
}

#[test]
fn a_board_that_failed_is_not_retried_behind_the_users_back() {
    // An error is already on screen with "press r to retry" under it. Asking
    // again on its own would hide a broken Odoo behind a spinner.
    let kicks = Kicks::default();
    let failed = Snapshot {
        board_error: Some("Odoo said no".to_string()),
        deploy: Some(DeployBoard::default()),
        ..Snapshot::default()
    };
    assert_eq!(kicks.claim(&failed), (false, false));
}
