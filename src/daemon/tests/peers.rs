//! Auto QA across your machines: a project is on in one place only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::daemon::{
    ask_peer, paused_projects, secrets_match, serve_peers, Paused, PeerAnswer, PEER_PATH,
};
use crate::odoo::QaStageTask;

const SECRET: &str = "a-shared-secret-of-length";

fn answer(id: &str, projects: &[&str]) -> PeerAnswer {
    PeerAnswer {
        id: id.to_string(),
        projects: projects.iter().map(|p| p.to_string()).collect(),
    }
}

fn mine(projects: &[&str]) -> Vec<String> {
    projects.iter().map(|p| p.to_string()).collect()
}

// --- the tie-break -------------------------------------------------------------

/// Both machines apply this to the same two answers, so they agree: the one
/// whose address sorts first keeps the project.
#[test]
fn the_machine_whose_address_sorts_first_keeps_a_shared_project() {
    let peers = vec![("laptop-2".to_string(), answer("100.0.0.1", &["Aurora"]))];
    assert_eq!(
        paused_projects(Some("100.0.0.2"), &mine(&["Aurora", "Borealis"]), &peers),
        vec![Paused {
            project: "Aurora".to_string(),
            peer: "laptop-2".to_string(),
        }]
    );
    // The other side of the same pair keeps it.
    let peers = vec![("laptop-1".to_string(), answer("100.0.0.2", &["aurora "]))];
    assert!(paused_projects(Some("100.0.0.1"), &mine(&["Aurora"]), &peers).is_empty());
}

#[test]
fn without_its_own_address_a_machine_pauses_a_shared_project() {
    let peers = vec![("laptop-2".to_string(), answer("100.0.0.9", &["Aurora"]))];
    assert_eq!(paused_projects(None, &mine(&["Aurora"]), &peers).len(), 1);
}

#[test]
fn a_secret_must_match_exactly() {
    assert!(secrets_match(SECRET, SECRET));
    assert!(!secrets_match("a-shared-secret-of-lengtH", SECRET));
    assert!(!secrets_match("", SECRET));
    assert!(!secrets_match(&format!("{SECRET}x"), SECRET));
}

// --- the listener --------------------------------------------------------------

fn listen(answer: PeerAnswer) -> crate::daemon::PeerListener {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    serve_peers(listener, SECRET.to_string(), move || answer.clone()).unwrap()
}

#[test]
fn the_listener_answers_with_the_secret_and_nothing_else() {
    let peer = listen(answer("100.0.0.1", &["Aurora"]));
    let port = peer.port();

    assert_eq!(
        ask_peer("127.0.0.1", port, SECRET),
        Some(answer("100.0.0.1", &["Aurora"]))
    );
    assert_eq!(ask_peer("127.0.0.1", port, "not-the-secret-at-all"), None);

    let wrong = crate::daemon::client::request_to(
        "127.0.0.1",
        port,
        "POST",
        PEER_PATH,
        Some(r#"{"secret":"nope"}"#),
        std::time::Duration::from_secs(2),
    )
    .expect("a response");
    assert_eq!(wrong.status, 403);
    // The daemon's own routes are not here.
    for path in ["/state", "/notifications/clear", "/auto-qa/joined"] {
        let other = crate::daemon::client::request_to(
            "127.0.0.1",
            port,
            "POST",
            path,
            Some(&json!({ "secret": SECRET }).to_string()),
            std::time::Duration::from_secs(2),
        )
        .expect("a response");
        assert_eq!(other.status, 404, "{path}");
    }
    peer.stop();
}

// --- through the Auto QA watcher ----------------------------------------------

fn entry(id: i64) -> QaStageTask {
    QaStageTask {
        task: Task {
            project_name: "Aurora".to_string(),
            ..a_task(id, "QA")
        },
        user_ids: Vec::new(),
        assigned_to_me: false,
        stage_entered: "2026-09-30 09:00:00".to_string(),
    }
}

fn watcher_with_peer(peer_port: u16) -> (TestEngine, Arc<Mutex<Vec<QaStageTask>>>) {
    let queue = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let fetch_queue = Arc::clone(&queue);
    let harness = engine_with(Setup {
        config: Some(json!({
            "qa": {
                "autoQa": ["Aurora"],
                "peers": ["127.0.0.1"],
                "peerPort": peer_port,
                "peerSecret": SECRET,
            }
        })),
        qa_stage: Some(Box::new(move |_stages| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fetch_queue.lock().unwrap().clone())
        })),
        ..Setup::default()
    });
    (harness, queue)
}

fn titles(harness: &TestEngine) -> Vec<String> {
    harness
        .state()
        .notifications
        .iter()
        .map(|n| n.title.clone())
        .collect()
}

/// The other machine keeps the project, so this one publishes nothing for it,
/// says so once, and takes it back when the other machine lets it go.
#[test]
fn a_project_another_machine_keeps_is_paused_here_and_said_once() {
    let peer_projects = Arc::new(Mutex::new(vec!["Aurora".to_string()]));
    let reported = Arc::clone(&peer_projects);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = serve_peers(listener, SECRET.to_string(), move || PeerAnswer {
        id: "100.0.0.1".to_string(),
        projects: reported.lock().unwrap().clone(),
    })
    .unwrap();

    let (harness, queue) = watcher_with_peer(peer.port());
    harness.engine.set_peer_id(Some("100.0.0.2".to_string()));
    harness.inner().watch_auto_qa();
    queue.lock().unwrap().push(entry(2));
    harness.inner().watch_auto_qa();
    harness.inner().watch_auto_qa();

    assert!(
        harness.state().auto_qa.arrivals.is_empty(),
        "a paused project started"
    );
    assert_eq!(
        titles(&harness),
        ["Auto QA for Aurora is on at two machines"],
        "the conflict was not said exactly once"
    );

    // Switched off at the other machine: this one keeps it again. The task
    // that arrived during the pause was the other machine's to start, so it
    // does not start here. The next arrival does.
    peer_projects.lock().unwrap().clear();
    harness.inner().watch_auto_qa();
    assert!(
        harness.state().auto_qa.arrivals.is_empty(),
        "a task the other machine started would start again here"
    );
    queue.lock().unwrap().push(entry(3));
    harness.inner().watch_auto_qa();
    let arrivals: Vec<i64> = harness
        .state()
        .auto_qa
        .arrivals
        .iter()
        .map(|a| a.task_id)
        .collect();
    assert_eq!(arrivals, vec![3]);
    peer.stop();
}

/// This machine sorts first, so it keeps the project and says nothing.
#[test]
fn the_machine_that_keeps_a_project_carries_on() {
    let peer = listen(answer("100.0.0.9", &["Aurora"]));
    let (harness, queue) = watcher_with_peer(peer.port());
    harness.engine.set_peer_id(Some("100.0.0.2".to_string()));
    harness.inner().watch_auto_qa();
    queue.lock().unwrap().push(entry(2));
    harness.inner().watch_auto_qa();

    assert_eq!(harness.state().auto_qa.arrivals.len(), 1);
    assert!(titles(&harness).is_empty());
    peer.stop();
}

/// A peer that does not answer is asleep or offline. This machine carries on.
#[test]
fn a_peer_that_does_not_answer_changes_nothing() {
    let unused = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    let (harness, queue) = watcher_with_peer(port);
    harness.inner().watch_auto_qa();
    queue.lock().unwrap().push(entry(2));
    harness.inner().watch_auto_qa();
    assert_eq!(harness.state().auto_qa.arrivals.len(), 1);
}
