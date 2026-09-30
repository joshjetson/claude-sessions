//! Auto QA, daemon side: which tasks the dashboard is told to start, and that
//! an arrival waits until the dashboard confirms it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::server::{post, served};
use super::*;
use crate::daemon::{AutoQaFeed, EngineEvent};
use crate::odoo::QaStageTask;

const PROJECT: &str = "Aurora";

fn entry(id: i64, project: &str) -> QaStageTask {
    QaStageTask {
        task: Task {
            project_name: project.to_string(),
            ..a_task(id, "QA")
        },
        user_ids: Vec::new(),
        assigned_to_me: false,
        stage_entered: "2026-09-30 09:00:00".to_string(),
    }
}

fn scripted(
    queue: Arc<Mutex<Vec<QaStageTask>>>,
    calls: Arc<AtomicUsize>,
) -> crate::daemon::QaStageFetch {
    Box::new(move |_stages| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(queue.lock().unwrap().clone())
    })
}

struct Watcher {
    harness: TestEngine,
    queue: Arc<Mutex<Vec<QaStageTask>>>,
    calls: Arc<AtomicUsize>,
}

fn watcher(config: serde_json::Value, tasks: Vec<QaStageTask>) -> Watcher {
    let queue = Arc::new(Mutex::new(tasks));
    let calls = Arc::new(AtomicUsize::new(0));
    let harness = engine_with(Setup {
        config: Some(config),
        qa_stage: Some(scripted(Arc::clone(&queue), Arc::clone(&calls))),
        ..Setup::default()
    });
    Watcher {
        harness,
        queue,
        calls,
    }
}

fn auto_on() -> serde_json::Value {
    json!({ "qa": { "autoQa": [PROJECT], "otherQaUserIds": [77] } })
}

fn feed(harness: &TestEngine) -> AutoQaFeed {
    harness.state().auto_qa.clone()
}

fn arrival_ids(harness: &TestEngine) -> Vec<i64> {
    feed(harness).arrivals.iter().map(|a| a.task_id).collect()
}

#[test]
fn with_no_project_on_nothing_is_fetched() {
    let w = watcher(json!({}), vec![entry(1, PROJECT)]);
    w.harness.inner().watch_auto_qa();
    assert_eq!(w.calls.load(Ordering::SeqCst), 0);
    assert_eq!(feed(&w.harness), AutoQaFeed::default());
}

/// Switched on, the backlog already in QA is recorded silently: Auto QA starts
/// what arrives from then on. The tasks are still published, because the
/// dashboard needs their Odoo state.
#[test]
fn switching_on_records_the_backlog_and_starts_what_arrives_next() {
    let w = watcher(auto_on(), vec![entry(1, PROJECT)]);
    w.harness.inner().watch_auto_qa();
    assert!(arrival_ids(&w.harness).is_empty(), "the backlog arrived");
    assert_eq!(feed(&w.harness).tasks.len(), 1);

    w.queue.lock().unwrap().push(entry(2, PROJECT));
    w.harness.inner().watch_auto_qa();
    assert_eq!(arrival_ids(&w.harness), vec![2]);
    assert_eq!(feed(&w.harness).arrivals[0].project, PROJECT);
    assert_eq!(feed(&w.harness).arrivals[0].stage, "QA");
}

/// An arrival waits for the dashboard, however many ticks pass and across a
/// restart, and goes once the dashboard confirms it.
#[test]
fn an_arrival_waits_until_the_dashboard_confirms_it() {
    let w = watcher(auto_on(), Vec::new());
    w.harness.inner().watch_auto_qa();
    w.queue.lock().unwrap().push(entry(2, PROJECT));
    for _ in 0..3 {
        w.harness.inner().watch_auto_qa();
    }
    let arrivals = feed(&w.harness).arrivals;
    assert_eq!(arrivals.len(), 1);

    w.harness.inner().auto_qa_joined(&[arrivals[0].key.clone()]);
    assert!(arrival_ids(&w.harness).is_empty());
    w.harness.inner().watch_auto_qa();
    assert!(
        arrival_ids(&w.harness).is_empty(),
        "a confirmed arrival came back"
    );
}

/// Back in QA after a revision is a new round, so it arrives again.
#[test]
fn a_task_back_in_qa_arrives_again() {
    let w = watcher(auto_on(), vec![entry(3, PROJECT)]);
    w.harness.inner().watch_auto_qa();
    let mut back = entry(3, PROJECT);
    back.stage_entered = "2026-10-01 10:00:00".to_string();
    *w.queue.lock().unwrap() = vec![back];
    w.harness.inner().watch_auto_qa();
    assert_eq!(arrival_ids(&w.harness), vec![3]);
}

/// Only the Auto QA projects' QA stages, and never a task another reviewer
/// holds: two reviewers with Auto QA on for one project must not both start it.
#[test]
fn other_projects_other_stages_and_other_reviewers_tasks_are_left_out() {
    let w = watcher(auto_on(), Vec::new());
    w.harness.inner().watch_auto_qa();
    let mut theirs = entry(4, PROJECT);
    theirs.user_ids = vec![77];
    let mut shared = entry(5, PROJECT);
    shared.user_ids = vec![77];
    shared.assigned_to_me = true;
    let mut revision = entry(6, PROJECT);
    revision.task.stage_name = "Revision Required".to_string();
    *w.queue.lock().unwrap() = vec![entry(7, "Other"), theirs, shared, revision];

    w.harness.inner().watch_auto_qa();
    assert_eq!(arrival_ids(&w.harness), vec![5]);
    let tasks: Vec<i64> = feed(&w.harness).tasks.iter().map(|t| t.id).collect();
    assert_eq!(tasks, vec![5]);
}

/// The dashboard learns of a change as an `auto-qa` event, and a dashboard
/// that connects later finds the feed in the snapshot.
#[test]
fn the_feed_is_published_and_rides_the_snapshot() {
    let w = watcher(auto_on(), Vec::new());
    w.harness.inner().watch_auto_qa();
    let events = w.harness.engine.subscribe();
    w.queue.lock().unwrap().push(entry(2, PROJECT));
    w.harness.inner().watch_auto_qa();

    let published: Vec<AutoQaFeed> = events
        .try_iter()
        .filter_map(|event| match event {
            EngineEvent::AutoQa(feed) => Some(*feed),
            _ => None,
        })
        .collect();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].arrivals.len(), 1);
    assert_eq!(w.harness.engine.snapshot().auto_qa, published[0]);

    // No change, no event.
    w.harness.inner().watch_auto_qa();
    assert_eq!(events.try_iter().count(), 0);
}

#[test]
fn switching_every_project_off_empties_the_feed() {
    let w = watcher(auto_on(), vec![entry(1, PROJECT)]);
    w.harness.inner().watch_auto_qa();
    fs::write(&w.harness.paths.config_path, json!({}).to_string()).unwrap();
    w.harness.inner().sync_config();
    w.harness.inner().watch_auto_qa();
    assert_eq!(feed(&w.harness), AutoQaFeed::default());
}

#[test]
fn the_route_confirms_arrivals() {
    let served = served();
    fs::write(&served.paths.config_path, auto_on().to_string()).unwrap();
    served.engine.inner().sync_config();
    served.engine.inner().state().auto_qa = AutoQaFeed {
        tasks: Vec::new(),
        arrivals: vec![crate::daemon::AutoArrival {
            key: "autoqa:2:qa:x".to_string(),
            task_id: 2,
            project: PROJECT.to_string(),
            stage: "QA".to_string(),
        }],
    };
    let answer = post(
        served.port(),
        "/auto-qa/joined",
        &json!({ "keys": ["autoqa:2:qa:x"] }).to_string(),
    );
    assert_eq!(answer.status, 202);
    assert!(served.engine.inner().state().auto_qa.arrivals.is_empty());
    assert!(served.engine.db().was_alerted("autoqa:2:qa:x"));
}
