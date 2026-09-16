//! Environment variables that feed into config resolution, captured once.
//!
//! Read as a value, never in the middle of an accessor: tests build an
//! [`EnvOverrides`] literal instead of exporting variables, which is what makes
//! the config suite safe to run in parallel.
//!
//! The precedence differs per variable, and deliberately so — see each field.

use std::env;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvOverrides {
    /// `ODOO_URL` / `ODOO_DB` / `ODOO_USER` / `ODOO_PASSWORD` are *fallbacks*:
    /// config wins per field, so a stale exported value held by the launching
    /// tab cannot override an explicit setting.
    pub odoo_url: Option<String>,
    pub odoo_db: Option<String>,
    pub odoo_user: Option<String>,
    pub odoo_password: Option<String>,
    /// `GITLAB_HOST` *overrides* config — it is how you point one run at another
    /// instance without editing the file.
    pub gitlab_host: Option<String>,
    /// `OPTICS_API` / `OPTICS_TOKEN` override config for the same reason.
    pub optics_api: Option<String>,
    pub optics_token: Option<String>,
    /// `CLAUDE_SESSIONS_NOTIFY_PORT` outranks every configured port.
    pub notify_port: Option<u16>,
}

impl EnvOverrides {
    pub fn from_env() -> Self {
        EnvOverrides {
            odoo_url: env_string("ODOO_URL"),
            odoo_db: env_string("ODOO_DB"),
            odoo_user: env_string("ODOO_USER"),
            odoo_password: env_string("ODOO_PASSWORD"),
            gitlab_host: env_string("GITLAB_HOST"),
            optics_api: env_string("OPTICS_API"),
            optics_token: env_string("OPTICS_TOKEN"),
            notify_port: env_string("CLAUDE_SESSIONS_NOTIFY_PORT")
                .and_then(|v| v.trim().parse::<u16>().ok())
                .filter(|port| *port != 0),
        }
    }
}

/// An exported-but-empty variable counts as unset, matching JavaScript's
/// treatment of `""` as falsy in every one of these lookups.
fn env_string(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}
