//! What the board worker needs beyond a terminal driver.
//!
//! Assembled once on the UI thread and handed over, so the worker holds no
//! configuration of its own — there is no second copy to go stale after a
//! dialog edits the target branch. `None` for the Odoo client is a real
//! configuration: a dashboard with no credentials still drives terminals and
//! still launches sessions, and says so where a board would be.

use std::sync::Arc;

use crate::daemon::TaskBackend;
use crate::odoo::OdooClient;
use crate::paths::Paths;
use crate::types::NotificationLevel;

/// What the worker needs beyond a terminal driver.
///
/// `None` for the Odoo client is a real configuration: a dashboard with no
/// credentials still drives terminals, still launches sessions, and says so
/// where a board would be.
pub struct BoardServices {
    pub paths: Paths,
    pub odoo: Option<Arc<OdooClient>>,
    pub backend: Option<Arc<dyn TaskBackend>>,
    pub sounds: Sounds,
    /// The Optics client, when this install has an endpoint and a token.
    /// `None` is the normal case and simply means no coverage badges.
    pub optics: Option<Arc<crate::optics::OpticsClient>>,
    /// Short `HEAD`s per QA worktree. Shared with the UI thread, which reads it
    /// while building the task menu and never fills it — see
    /// [`crate::qaden::HeadCache`].
    pub qa_heads: Arc<crate::qaden::HeadCache>,
}

impl BoardServices {
    pub fn new(
        paths: Paths,
        odoo: Option<Arc<OdooClient>>,
        optics: Option<Arc<crate::optics::OpticsClient>>,
        sounds: Sounds,
    ) -> Self {
        let backend = odoo.clone().map(|client| {
            Arc::new(crate::daemon::OdooTaskBackend::new(client)) as Arc<dyn TaskBackend>
        });
        BoardServices {
            paths,
            odoo,
            backend,
            optics,
            sounds,
            qa_heads: Arc::new(crate::qaden::HeadCache::new()),
        }
    }

    /// Nothing configured: the dashboard still runs, the board says why it is
    /// empty.
    pub fn offline(paths: Paths) -> Self {
        BoardServices {
            paths,
            odoo: None,
            backend: None,
            optics: None,
            sounds: Sounds::default(),
            qa_heads: Arc::new(crate::qaden::HeadCache::new()),
        }
    }
}

/// Which file each notification level plays.
///
/// The Node app shipped an mp3 in the repo for the success chime and named the
/// three macOS system sounds for the rest. A bundled asset is not portable and
/// not something a crate should carry, so every level is configurable and the
/// defaults are all system sounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sounds {
    pub success: Option<String>,
    pub warn: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for Sounds {
    fn default() -> Self {
        Sounds {
            // Glass is the closest system sound to the "finished" chime the
            // Node app bundled; override it in config with a file of your own.
            success: Some("/System/Library/Sounds/Glass.aiff".into()),
            warn: Some("/System/Library/Sounds/Sosumi.aiff".into()),
            error: Some("/System/Library/Sounds/Basso.aiff".into()),
            info: Some("/System/Library/Sounds/Tink.aiff".into()),
        }
    }
}

impl Sounds {
    pub fn file(&self, level: NotificationLevel) -> Option<&str> {
        match level {
            NotificationLevel::Success => self.success.as_deref(),
            NotificationLevel::Warn => self.warn.as_deref(),
            NotificationLevel::Error => self.error.as_deref(),
            NotificationLevel::Info => self.info.as_deref(),
        }
    }
}
