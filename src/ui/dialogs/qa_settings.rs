//! The settings dialog's QA page: the role, QA runs, notifications, machine
//! health, Auto QA and the peer check, without opening the config file.
//!
//! Each change saves at once, like the chat page. The daemon re-reads the
//! config within a second, so a change applies on its next tick. The three
//! peer settings are the exception: the peer listener binds at daemon start,
//! and the page says so.

use crate::config::ConfigHandle;
use crate::qarun::RunMode;
use crate::types::UserRole;

/// One QA setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaField {
    Role,
    LaneLimit,
    AutoRefill,
    CoordinatorMode,
    Alerts,
    NotifyNewInQa,
    HealthGate,
    /// Auto QA for one project.
    AutoQa(String),
    Peers,
    PeerSecret,
    PeerPort,
}

/// A row on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaRow {
    Separator(&'static str),
    /// A line of explanation under a section.
    Note(&'static str),
    Field(QaField),
}

/// The highest lane limit the page offers. Higher is set in the file.
const MAX_LANES: usize = 20;

/// The page, built from the config: one Auto QA row per project mapped to a
/// repo folder, because those are the projects a start could reach.
pub fn rows(config: &ConfigHandle) -> Vec<QaRow> {
    let mut rows = vec![
        QaRow::Field(QaField::Role),
        QaRow::Separator("──── QA runs ────"),
        QaRow::Field(QaField::LaneLimit),
        QaRow::Field(QaField::AutoRefill),
        QaRow::Field(QaField::CoordinatorMode),
        QaRow::Separator("──── Notifications ────"),
        QaRow::Field(QaField::Alerts),
        QaRow::Field(QaField::NotifyNewInQa),
        QaRow::Separator("──── Machine health ────"),
        QaRow::Field(QaField::HealthGate),
        QaRow::Separator("──── Auto QA ────"),
    ];
    let mut projects: Vec<&str> = config.odoo_project_names().collect();
    projects.sort_unstable_by_key(|name| name.to_lowercase());
    if projects.is_empty() {
        rows.push(QaRow::Note("Map a project to a repo folder first."));
    }
    for project in projects {
        rows.push(QaRow::Field(QaField::AutoQa(project.to_string())));
    }
    rows.extend([
        QaRow::Separator("──── Your other machines ────"),
        QaRow::Field(QaField::Peers),
        QaRow::Field(QaField::PeerSecret),
        QaRow::Field(QaField::PeerPort),
        QaRow::Note("Peer settings apply when the daemon restarts."),
    ]);
    rows
}

/// The rows `↑↓` walks.
pub fn fields(config: &ConfigHandle) -> Vec<QaField> {
    rows(config)
        .into_iter()
        .filter_map(|row| match row {
            QaRow::Field(field) => Some(field),
            _ => None,
        })
        .collect()
}

fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_string()
}

impl QaField {
    pub fn label(&self) -> String {
        match self {
            QaField::Role => "Role".to_string(),
            QaField::LaneLimit => "Lane limit".to_string(),
            QaField::AutoRefill => "Refill lanes".to_string(),
            QaField::CoordinatorMode => "Coordinator".to_string(),
            QaField::Alerts => "Alerts".to_string(),
            QaField::NotifyNewInQa => "New in QA".to_string(),
            QaField::HealthGate => "Health gate".to_string(),
            QaField::AutoQa(project) => project.clone(),
            QaField::Peers => "Peers".to_string(),
            QaField::PeerSecret => "Peer secret".to_string(),
            QaField::PeerPort => "Peer port".to_string(),
        }
    }

    /// The fields edited as text with Enter. The rest change with `←→`.
    pub fn is_text(&self) -> bool {
        matches!(self, QaField::Peers | QaField::PeerSecret)
    }

    pub fn display(&self, config: &ConfigHandle) -> String {
        match self {
            QaField::Role => config.role().as_str().to_string(),
            QaField::LaneLimit => config
                .qa_lane_limit()
                .map_or("none".to_string(), |limit| limit.to_string()),
            QaField::AutoRefill => on_off(config.qa_auto_refill()),
            QaField::CoordinatorMode => match config.qa_coordinator_mode() {
                RunMode::Triage => "triage".to_string(),
                RunMode::Shadow => "shadow".to_string(),
            },
            QaField::Alerts => on_off(config.alerts().enabled),
            QaField::NotifyNewInQa => on_off(config.qa_notify_new_in_qa()),
            QaField::HealthGate => on_off(config.qa_health_gate()),
            QaField::AutoQa(project) => {
                let on = config.qa_auto(project);
                if !on && config.odoo_project_dir_list(project).len() != 1 {
                    "off · needs one folder".to_string()
                } else {
                    on_off(on)
                }
            }
            QaField::Peers => {
                let hosts = config.qa_peers().hosts;
                if hosts.is_empty() {
                    "none".to_string()
                } else {
                    hosts.join(", ")
                }
            }
            QaField::PeerSecret => match config.qa_peer_secret_raw() {
                None | Some("") => "unset".to_string(),
                Some(secret) if secret.len() < crate::daemon::MIN_PEER_SECRET_LEN => {
                    "too short".to_string()
                }
                Some(secret) => {
                    let tail: String = secret
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("••••{tail}")
                }
            },
            QaField::PeerPort => config.qa_peers().port.to_string(),
        }
    }

    /// What Enter puts in the edit box. The secret starts empty: showing it
    /// would put it on screen.
    pub fn raw_text(&self, config: &ConfigHandle) -> String {
        match self {
            QaField::Peers => config.qa_peers().hosts.join(", "),
            _ => String::new(),
        }
    }

    /// Save a text edit. `Err` is a message for the page.
    pub fn set_text(&self, config: &mut ConfigHandle, value: &str) -> Result<(), String> {
        match self {
            QaField::Peers => {
                let hosts: Vec<String> = value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .map(str::trim)
                    .filter(|host| !host.is_empty())
                    .map(str::to_string)
                    .collect();
                config
                    .update_qa(|qa| qa.peers = (!hosts.is_empty()).then_some(hosts))
                    .map_err(|e| e.to_string())
            }
            QaField::PeerSecret => {
                let secret = value.trim().to_string();
                if !secret.is_empty() && secret.len() < crate::daemon::MIN_PEER_SECRET_LEN {
                    return Err(format!(
                        "A peer secret needs {} characters or more. Press g to generate one.",
                        crate::daemon::MIN_PEER_SECRET_LEN
                    ));
                }
                config
                    .update_qa(|qa| qa.peer_secret = (!secret.is_empty()).then_some(secret))
                    .map_err(|e| e.to_string())
            }
            _ => Ok(()),
        }
    }

    /// Change a `←→` field one step. `Err` is a message for the page.
    pub fn cycle(&self, config: &mut ConfigHandle, dir: i32) -> Result<(), String> {
        let saved = |result: std::io::Result<()>| result.map_err(|e| e.to_string());
        match self {
            QaField::Role => {
                let roles = [UserRole::Dev, UserRole::Qa, UserRole::Pm];
                let at = roles.iter().position(|r| *r == config.role()).unwrap_or(0) as i32;
                let next = roles[(at + dir).rem_euclid(roles.len() as i32) as usize];
                saved(config.set_role(next))
            }
            QaField::LaneLimit => {
                let now = config.qa_lane_limit().unwrap_or(0) as i32;
                let next = (now + dir).clamp(0, MAX_LANES as i32) as usize;
                saved(config.update_qa(|qa| qa.lane_limit = (next > 0).then_some(next)))
            }
            QaField::AutoRefill => {
                let next = !config.qa_auto_refill();
                saved(config.update_qa(|qa| qa.auto_refill = Some(next)))
            }
            QaField::CoordinatorMode => {
                let shadow = config.qa_coordinator_mode() == RunMode::Triage;
                saved(
                    config
                        .update_qa(|qa| qa.coordinator_mode = shadow.then(|| "shadow".to_string())),
                )
            }
            QaField::Alerts => {
                let next = !config.alerts().enabled;
                saved(config.set_alerts_enabled(next))
            }
            QaField::NotifyNewInQa => {
                let next = !config.qa_notify_new_in_qa();
                saved(config.update_qa(|qa| qa.notify_new_in_qa = Some(next)))
            }
            QaField::HealthGate => {
                let next = !config.qa_health_gate();
                saved(config.update_qa(|qa| qa.health_gate = Some(next)))
            }
            QaField::AutoQa(project) => {
                let on = !config.qa_auto(project);
                let folders = config.odoo_project_dir_list(project).len();
                if on && folders != 1 {
                    return Err(format!(
                        "Auto QA needs exactly one repo folder for {project}, and it has \
                         {folders}. Set it with \"Repo folders…\" in a task's menu."
                    ));
                }
                saved(config.set_auto_qa(project, on))
            }
            QaField::PeerPort => {
                let now = config.qa_peers().port as i32;
                let next = (now + dir).clamp(1024, 65535) as u16;
                let default = crate::daemon::DEFAULT_PEER_PORT;
                saved(config.update_qa(|qa| qa.peer_port = (next != default).then_some(next)))
            }
            QaField::Peers | QaField::PeerSecret => Ok(()),
        }
    }
}

/// A fresh peer secret: 32 hex characters from the system's random source.
/// `None` where there is none to read, and the person types one instead.
pub fn generate_secret() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
