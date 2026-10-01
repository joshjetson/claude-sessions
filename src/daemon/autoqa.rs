//! Auto QA: tasks that arrive in a QA stage of an Auto QA project.
//!
//! The daemon finds them and the dashboard starts them. Runs, their lanes and
//! the launch itself all live in the dashboard, so this side does two things:
//!
//! - It publishes the tasks sitting in the QA stages of the Auto QA projects,
//!   with their Odoo state. The reviewer's board shows their own tasks, so a
//!   task in QA assigned to nobody is not on it, and a run could not start a
//!   task it cannot see.
//! - It holds each new arrival until the dashboard says the task has joined
//!   its run. An arrival while no dashboard is open waits, rather than being
//!   recorded as handled and lost.
//!
//! An arrival is keyed by task, stage and stage-entry time, like the QA arrival
//! notification, so a task that comes back to QA after a revision is new work
//! again. The first time a project is switched on, whatever already sits in
//! its QA stages is recorded silently: Auto QA starts what arrives from then
//! on, not the backlog.

use serde::{Deserialize, Serialize};

use crate::config::QaAlertConfig;
use crate::odoo::QaStageTask;
use crate::scan::ProcessSource;
use crate::types::Task;

use super::alerts::normalise_stage;
use super::engine::EngineInner;
use super::events::EngineEvent;

/// What the dashboard needs for Auto QA. Also the `auto-qa` event's payload
/// and a field of the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoQaFeed {
    /// Every task in a QA stage of an Auto QA project, with its Odoo state.
    pub tasks: Vec<Task>,
    /// Arrivals the dashboard has not yet confirmed.
    pub arrivals: Vec<AutoArrival>,
}

/// One task that arrived in a QA stage of an Auto QA project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoArrival {
    /// What the dashboard sends back once the task has joined its run.
    pub key: String,
    pub task_id: i64,
    pub project: String,
    pub stage: String,
}

/// Marks that a project's QA stages have been recorded once.
fn bootstrap_key(project: &str) -> String {
    format!("autoqa:bootstrapped:{}", project.trim().to_lowercase())
}

/// The key that records one arrival as handled.
pub fn arrival_key(entry: &QaStageTask) -> String {
    format!(
        "autoqa:{}:{}:{}",
        entry.task.id,
        normalise_stage(&entry.task.stage_name),
        entry.stage_entered
    )
}

/// The tasks in QA stages that belong to an Auto QA project and are not
/// another reviewer's.
///
/// A task assigned to another QA reviewer and not to you is theirs, the same
/// rule the arrival notification uses. It is left out, so two reviewers with
/// Auto QA on for one project do not both start it.
pub fn auto_tasks<'a>(
    tasks: &'a [QaStageTask],
    projects: &[String],
    rule: &QaAlertConfig,
) -> Vec<&'a QaStageTask> {
    let watched: Vec<String> = rule.stages.iter().map(|s| normalise_stage(s)).collect();
    let is_auto = |name: &str| {
        let name = name.trim().to_lowercase();
        projects.iter().any(|p| p.trim().to_lowercase() == name)
    };
    tasks
        .iter()
        .filter(|entry| entry.task.id != 0)
        .filter(|entry| is_auto(&entry.task.project_name))
        .filter(|entry| watched.contains(&normalise_stage(&entry.task.stage_name)))
        .filter(|entry| {
            entry.assigned_to_me
                || !entry
                    .user_ids
                    .iter()
                    .any(|id| rule.other_qa_user_ids.contains(id))
        })
        .collect()
}

impl<S: ProcessSource> EngineInner<S> {
    /// Refresh the Auto QA feed: one Odoo query on the slow tick, and only
    /// when a project has Auto QA on.
    pub(crate) fn watch_auto_qa(&self) {
        let (projects, rule) = {
            let config = self.config();
            (config.qa_auto_projects(), config.qa_alerts())
        };
        let kept = self.keep_unless_a_peer_has(projects.clone());
        if projects.is_empty() || rule.stages.is_empty() {
            self.set_auto_qa(AutoQaFeed::default());
            return;
        }
        let Some(fetch) = &self.fetch_qa_stage else {
            return;
        };
        // Best-effort: an Odoo blip keeps the last feed rather than emptying it.
        let Ok(tasks) = fetch(&rule.stages) else {
            return;
        };

        // A project another machine keeps: that machine starts what arrives,
        // so it is recorded here as handled. Otherwise, when this machine
        // took the project back, it would start each of those tasks again.
        let paused: Vec<String> = projects
            .iter()
            .filter(|project| !kept.contains(project))
            .cloned()
            .collect();
        for entry in auto_tasks(&tasks, &paused, &rule) {
            self.db.mark_alerted(&arrival_key(entry));
        }
        for project in &paused {
            self.db.mark_alerted(&bootstrap_key(project));
        }

        let projects = kept;
        if projects.is_empty() {
            self.set_auto_qa(AutoQaFeed::default());
            return;
        }
        let mine = auto_tasks(&tasks, &projects, &rule);

        for project in &projects {
            let key = bootstrap_key(project);
            if self.db.was_alerted(&key) {
                continue;
            }
            let wanted = project.trim().to_lowercase();
            for entry in mine
                .iter()
                .filter(|entry| entry.task.project_name.trim().to_lowercase() == wanted)
            {
                self.db.mark_alerted(&arrival_key(entry));
            }
            self.db.mark_alerted(&key);
        }

        let arrivals = mine
            .iter()
            .map(|entry| (arrival_key(entry), entry))
            .filter(|(key, _)| !self.db.was_alerted(key))
            .map(|(key, entry)| AutoArrival {
                key,
                task_id: entry.task.id,
                project: entry.task.project_name.clone(),
                stage: entry.task.stage_name.clone(),
            })
            .collect();
        self.set_auto_qa(AutoQaFeed {
            tasks: mine.iter().map(|entry| entry.task.clone()).collect(),
            arrivals,
        });
    }

    /// The projects this machine keeps, after asking your other machines.
    ///
    /// A project another machine keeps is dropped for this pass, and the first
    /// time that happens a notification says so: it needs the person, because
    /// only they can switch it off at one machine. See [`super::peers`].
    fn keep_unless_a_peer_has(&self, projects: Vec<String>) -> Vec<String> {
        let peers = self.config().qa_peers();
        let (Some(secret), false) = (peers.secret.as_deref(), peers.hosts.is_empty()) else {
            return projects;
        };
        if projects.is_empty() {
            self.state().peer_paused.clear();
            return projects;
        }
        let answers: Vec<(String, super::peers::PeerAnswer)> = peers
            .hosts
            .iter()
            .filter_map(|host| {
                super::peers::ask_peer(host, peers.port, secret)
                    .map(|answer| (host.clone(), answer))
            })
            .collect();
        let my_id = self.state().peer_id.clone();
        let paused = super::peers::paused_projects(my_id.as_deref(), &projects, &answers);

        let newly: Vec<super::peers::Paused> = {
            let mut state = self.state();
            let now: std::collections::BTreeSet<String> =
                paused.iter().map(|p| p.project.to_lowercase()).collect();
            let newly = paused
                .iter()
                .filter(|p| !state.peer_paused.contains(&p.project.to_lowercase()))
                .cloned()
                .collect();
            state.peer_paused = now;
            newly
        };
        for conflict in newly {
            self.raise_notification(super::notify::NewNotification {
                project: Some(conflict.project.clone()),
                level: crate::types::NotificationLevel::Warn,
                ..super::notify::NewNotification::new(
                    "auto-qa",
                    format!("Auto QA for {} is on at two machines", conflict.project),
                    format!(
                        "{} has Auto QA on for {} too, and keeps it. This machine paused it so \
                         no task starts twice. Switch it off at one of them with A on the board.",
                        conflict.peer, conflict.project
                    ),
                )
            });
        }
        projects
            .into_iter()
            .filter(|project| {
                !paused
                    .iter()
                    .any(|p| p.project.eq_ignore_ascii_case(project))
            })
            .collect()
    }

    /// The dashboard put these arrivals in their runs. Record them, so they
    /// are not handed over again.
    pub(crate) fn auto_qa_joined(&self, keys: &[String]) {
        for key in keys.iter().filter(|key| key.starts_with("autoqa:")) {
            self.db.mark_alerted(key);
        }
        let mut feed = self.state().auto_qa.clone();
        feed.arrivals.retain(|arrival| !keys.contains(&arrival.key));
        self.set_auto_qa(feed);
    }

    /// Store the feed, and tell the dashboards when it changed.
    fn set_auto_qa(&self, feed: AutoQaFeed) {
        {
            let mut state = self.state();
            if state.auto_qa == feed {
                return;
            }
            state.auto_qa = feed.clone();
        }
        self.publish(EngineEvent::AutoQa(Box::new(feed)));
    }
}
