//! Reading the config: every accessor that turns the stored file plus the
//! environment into the value a caller actually wants.
//!
//! Each one is a field read over the cached [`Config`]. The Node original
//! re-read and re-parsed the JSON file inside every one of these.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use super::{
    expand_tilde, lookup_ci, ChatConfig, ConfigHandle, Group, OneOrMany, DEFAULT_HIDE_STAGE,
    DEFAULT_HIDE_STATES, DEFAULT_NEW_TASK_STAGE, DEFAULT_PORT, DEFAULT_TMUX_SESSION,
};
use crate::types::{DefaultView, OdooCreds, TerminalDriverName};

impl ConfigHandle {
    // --- sessions view ------------------------------------------------------

    pub fn chat(&self) -> &ChatConfig {
        &self.config.chat
    }

    /// The configured groups, with a leading `~` expanded against the home
    /// directory.
    ///
    /// Expansion happens HERE, once, rather than at each comparison, because
    /// every consumer matches the path against an absolute working directory:
    /// the sessions tree, the daemon's folder discovery, the local feed's, and
    /// the journal's repo search. [`ConfigHandle::add_group`] expands on write,
    /// so a group added through the dialog is already absolute — but a
    /// hand-written `"groups": [{"path": "~/dev"}]` is not, and it matched
    /// nothing at all: every session under it fell through to "Other sessions"
    /// and the group rendered empty.
    ///
    /// The stored value is left alone, so the file keeps the spelling its owner
    /// wrote.
    pub fn groups(&self) -> Vec<Group> {
        self.config
            .groups
            .iter()
            .map(|group| Group {
                path: expand_tilde(&group.path, &self.home),
                ..group.clone()
            })
            .collect()
    }

    /// Which tab the dashboard opens on. Defaults to the board; an unrecognised
    /// value falls back rather than failing.
    pub fn default_view(&self) -> DefaultView {
        match self.config.default_view.as_deref() {
            Some("sessions") => DefaultView::Sessions,
            Some("deploy") => DefaultView::Deploy,
            _ => DefaultView::Board,
        }
    }

    pub fn nicknames(&self) -> &BTreeMap<String, String> {
        &self.config.nicknames
    }

    /// Session ids are exact — no case folding, they are UUIDs.
    pub fn session_nickname(&self, session_id: &str) -> Option<&str> {
        self.config.nicknames.get(session_id).map(String::as_str)
    }

    // --- integrations -------------------------------------------------------

    /// Config wins per field over `ODOO_*`, so a stale exported value cannot
    /// override an explicit setting; the env fills only the gaps.
    pub fn odoo_creds(&self) -> OdooCreds {
        let block = self.config.odoo.as_ref();
        let pick = |configured: Option<&String>, from_env: Option<&String>| -> String {
            configured
                .map(String::as_str)
                .filter(|v| !v.is_empty())
                .or(from_env.map(String::as_str))
                .unwrap_or_default()
                .to_string()
        };
        let url = pick(
            block.and_then(|o| o.url.as_ref()),
            self.env.odoo_url.as_ref(),
        );
        OdooCreds {
            url: url.trim_end_matches('/').to_string(),
            db: pick(block.and_then(|o| o.db.as_ref()), self.env.odoo_db.as_ref()),
            user: pick(
                block.and_then(|o| o.user.as_ref()),
                self.env.odoo_user.as_ref(),
            ),
            password: pick(
                block.and_then(|o| o.password.as_ref()),
                self.env.odoo_password.as_ref(),
            ),
        }
    }

    /// `GITLAB_HOST` overrides config. There is no built-in default: this is a
    /// public build and the only correct host is the configured one.
    pub fn gitlab_host(&self) -> Option<&str> {
        first_non_empty([
            self.env.gitlab_host.as_deref(),
            self.config.gitlab_host.as_deref(),
        ])
    }

    /// Optics endpoint. `OPTICS_API` overrides config; unset means the coverage
    /// badges are simply off.
    pub fn optics_api(&self) -> Option<&str> {
        first_non_empty([
            self.env.optics_api.as_deref(),
            self.config.optics.as_ref().and_then(|o| o.api.as_deref()),
        ])
        .map(|api| api.trim_end_matches('/'))
    }

    /// Optics token. No default, ever — a shipped credential is a leaked one.
    pub fn optics_token(&self) -> Option<&str> {
        first_non_empty([
            self.env.optics_token.as_deref(),
            self.config.optics.as_ref().and_then(|o| o.token.as_deref()),
        ])
    }

    /// Odoo project name -> Optics sdk_key, when the names do not match.
    pub fn optics_project(&self, project: &str) -> Option<&str> {
        let projects = &self.config.optics.as_ref()?.projects;
        lookup_ci(projects, project).map(|(_, key)| key.as_str())
    }

    /// The whole Odoo-name -> Optics-key map, for a client that resolves names
    /// itself rather than asking per project.
    pub fn optics_projects(&self) -> HashMap<String, String> {
        self.config
            .optics
            .as_ref()
            .map(|optics| {
                optics
                    .projects
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Explicit project -> ssh alias overrides, for projects whose name does not
    /// resemble their host.
    pub fn ssh_hosts(&self) -> &BTreeMap<String, String> {
        &self.config.ssh_hosts
    }

    pub fn ssh_host(&self, project: &str) -> Option<&str> {
        lookup_ci(&self.config.ssh_hosts, project).map(|(_, alias)| alias.as_str())
    }

    // --- stages, branches, repos -------------------------------------------

    /// Stage a finished task moves to, tried in order. `None` means "work it out
    /// from the project's stages" — never guess forward blindly.
    pub fn done_stage(&self) -> Option<&[String]> {
        self.config.done_stage.as_ref().map(OneOrMany::as_slice)
    }

    /// Stage a task moves to when work starts on it (or resumes for a revision).
    pub fn in_progress_stage(&self) -> Option<&[String]> {
        self.config
            .in_progress_stage
            .as_ref()
            .map(OneOrMany::as_slice)
    }

    /// Per-project merge-request target branch. `None` leaves the decision to
    /// the repository default.
    /// How many QA sessions a run may have live at once, or `None` for no cap.
    ///
    /// A configured `0` reads as no cap too: the field exists to impose a limit,
    /// so the absence of one and an explicit zero mean the same thing.
    pub fn qa_lane_limit(&self) -> Option<usize> {
        self.config
            .qa
            .as_ref()
            .and_then(|qa| qa.lane_limit)
            .filter(|limit| *limit > 0)
    }

    /// Whether the sessions tree draws a group's quiet folders. Off unless the
    /// config says otherwise — see [`crate::config::SessionsBlock`].
    pub fn show_inactive_folders(&self) -> bool {
        self.config
            .sessions
            .as_ref()
            .and_then(|s| s.show_inactive_folders)
            .unwrap_or(false)
    }

    pub fn target_branch(&self, project: &str) -> Option<&str> {
        lookup_ci(&self.config.target_branches, project).map(|(_, branch)| branch.as_str())
    }

    /// An Odoo project can map to several local repo folders; a legacy single
    /// string reads as a one-element list.
    pub fn odoo_project_dir_list(&self, project: &str) -> &[String] {
        lookup_ci(&self.config.odoo_project_dirs, project)
            .map(|(_, dirs)| dirs.as_slice())
            .unwrap_or_default()
    }

    pub fn odoo_project_names(&self) -> impl Iterator<Item = &str> {
        self.config.odoo_project_dirs.keys().map(String::as_str)
    }

    // --- board --------------------------------------------------------------

    /// `include`: when non-empty the "all" view loads only these projects.
    /// `ignore`: hidden in both "mine" and "all".
    pub fn board_project_filter(&self) -> BoardProjectFilter {
        let board = self.config.board.as_ref();
        BoardProjectFilter {
            include: board.map(|b| b.include.clone()).unwrap_or_default(),
            ignore: board.map(|b| b.ignore.clone()).unwrap_or_default(),
        }
    }

    /// The per-level notification sounds, with the system defaults filled in.
    pub fn sounds(&self) -> crate::ui::actions::Sounds {
        let block = self.config.sounds.as_ref();
        if block.and_then(|block| block.enabled) == Some(false) {
            return crate::ui::actions::Sounds {
                success: None,
                warn: None,
                error: None,
                info: None,
            };
        }
        let default = crate::ui::actions::Sounds::default();
        let pick = |configured: Option<&String>, fallback: Option<String>| {
            configured.cloned().filter(|s| !s.is_empty()).or(fallback)
        };
        crate::ui::actions::Sounds {
            success: pick(block.and_then(|b| b.success.as_ref()), default.success),
            warn: pick(block.and_then(|b| b.warn.as_ref()), default.warn),
            error: pick(block.and_then(|b| b.error.as_ref()), default.error),
            info: pick(block.and_then(|b| b.info.as_ref()), default.info),
        }
    }

    pub fn board_hide_filter(&self) -> BoardHideFilter {
        let board = self.config.board.as_ref();
        let hide_states = match board.and_then(|b| b.hide_states.as_ref()) {
            Some(states) => states.clone(),
            None => DEFAULT_HIDE_STATES
                .iter()
                .filter(|state| {
                    // Legacy toggle, still honoured: hideDoneState:false keeps
                    // Done visible.
                    **state != "1_done" || board.and_then(|b| b.hide_done_state) != Some(false)
                })
                .map(|state| state.to_string())
                .collect(),
        };
        BoardHideFilter {
            hide_stages: board
                .and_then(|b| b.hide_stages.clone())
                .unwrap_or_else(|| vec![DEFAULT_HIDE_STAGE.to_string()]),
            hide_states,
        }
    }

    // --- deploy -------------------------------------------------------------

    /// The Deploy tab is opt-in: a project appears there only once it has an
    /// entry here.
    pub fn deploy_project_names(&self) -> impl Iterator<Item = &str> {
        self.config
            .deploy
            .as_ref()
            .into_iter()
            .flat_map(|d| d.projects.keys().map(String::as_str))
    }

    pub fn deploy_project_config(&self, project: &str) -> Option<ResolvedDeployConfig> {
        let projects = &self.config.deploy.as_ref()?.projects;
        let (key, raw) = lookup_ci(projects, project)?;
        let settings = raw.settings();
        // `~` is expanded on read here (and on write for groups) because a deploy
        // cwd is routinely hand-written into the config file.
        let cwd = settings
            .cwd
            .filter(|c| !c.is_empty())
            .or_else(|| self.odoo_project_dir_list(key).first().cloned())
            .map(|c| PathBuf::from(expand_tilde(&c, &self.home)));
        Some(ResolvedDeployConfig {
            command: settings.command.unwrap_or_default(),
            target_branch: settings
                .target_branch
                .filter(|b| !b.is_empty())
                .or_else(|| self.target_branch(key).map(str::to_string))
                .unwrap_or_else(|| "main".to_string()),
            project: key.to_string(),
            cwd,
        })
    }

    // --- daemon, alerts, usage ---------------------------------------------

    /// Port precedence, preserved from the Node daemon: an explicit `--port`
    /// first, then `CLAUDE_SESSIONS_NOTIFY_PORT`, then `daemon.port`, then the
    /// legacy `notifyPort`, then whatever `notify.json` advertises, then 8787.
    pub fn resolve_port(&self, explicit: Option<u16>, discovered: Option<u16>) -> u16 {
        let configured = self
            .config
            .daemon
            .as_ref()
            .and_then(|d| d.port)
            .or(self.config.notify_port);
        [explicit, self.env.notify_port, configured, discovered]
            .into_iter()
            .flatten()
            .find(|port| *port != 0)
            .unwrap_or(DEFAULT_PORT)
    }

    /// Start a daemon when none is reachable. Default on.
    pub fn daemon_autostart(&self) -> bool {
        self.config.daemon.as_ref().and_then(|d| d.autostart) != Some(false)
    }

    pub fn daemon_enabled(&self) -> bool {
        self.config.daemon.as_ref().and_then(|d| d.enabled) != Some(false)
    }

    pub fn terminal_driver(&self) -> TerminalDriverName {
        match self
            .config
            .terminal
            .as_ref()
            .and_then(|t| t.driver.as_deref())
        {
            Some("tmux") => TerminalDriverName::Tmux,
            Some("iterm2") => TerminalDriverName::Iterm2,
            _ => TerminalDriverName::Auto,
        }
    }

    pub fn tmux_session(&self) -> &str {
        self.config
            .terminal
            .as_ref()
            .and_then(|t| t.tmux_session.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_TMUX_SESSION)
    }

    /// Alerts the daemon raises on its own, without anyone looking at the board.
    pub fn alerts(&self) -> AlertConfig {
        let alerts = self.config.alerts.as_ref();
        let minutes = |value: Option<&serde_json::Number>| value.and_then(|n| n.as_f64());
        AlertConfig {
            enabled: alerts.and_then(|a| a.enabled) != Some(false),
            new_task_stages: match alerts.map(|a| a.new_task_stages.clone()) {
                Some(stages) if !stages.is_empty() => stages,
                _ => vec![DEFAULT_NEW_TASK_STAGE.to_string()],
            },
            stuck_after: from_minutes(
                minutes(alerts.and_then(|a| a.stuck_after_minutes.as_ref())).filter(|m| *m > 0.0),
                15.0,
            ),
            // 0 is meaningful here — it means "flag it once, never remind".
            remind_every: from_minutes(
                minutes(alerts.and_then(|a| a.remind_every_minutes.as_ref())).filter(|m| *m >= 0.0),
                30.0,
            ),
        }
    }

    /// The usage readout shells out to `claude -p "/usage"`, which spends the
    /// quota it reports — hence "only when asked" as the default interval.
    pub fn usage(&self) -> UsageConfig {
        let usage = self.config.usage.as_ref();
        let minutes = usage
            .and_then(|u| u.interval_minutes.as_ref())
            .and_then(|n| n.as_f64())
            .filter(|m| *m > 0.0);
        UsageConfig {
            enabled: usage.and_then(|u| u.enabled) != Some(false),
            interval: minutes.map(minutes_to_duration),
        }
    }

    /// Whether to sample memory into `runtime/memory.log`. Either source turns
    /// it on; the default is off.
    pub fn diagnostics(&self) -> bool {
        self.env.diagnostics || self.config.diagnostics == Some(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDeployConfig {
    /// The key as stored in config, not as asked for.
    pub project: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub target_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertConfig {
    pub enabled: bool,
    /// Stages that mean "this is yours to pick up now".
    pub new_task_stages: Vec<String>,
    /// A running task session silent for this long is flagged.
    pub stuck_after: Duration,
    /// How often to re-flag a still-silent session; zero means once only.
    pub remind_every: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageConfig {
    pub enabled: bool,
    /// `None` is the default: fetched at startup and then only on request.
    pub interval: Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardProjectFilter {
    pub include: Vec<String>,
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardHideFilter {
    pub hide_stages: Vec<String>,
    pub hide_states: Vec<String>,
}

fn first_non_empty<const N: usize>(candidates: [Option<&str>; N]) -> Option<&str> {
    candidates
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
}

fn from_minutes(minutes: Option<f64>, fallback: f64) -> Duration {
    minutes_to_duration(minutes.unwrap_or(fallback))
}

fn minutes_to_duration(minutes: f64) -> Duration {
    Duration::from_secs_f64(minutes.max(0.0) * 60.0)
}
