//! Writing the config: every mutator, each of which updates the cache and the
//! file together so a later read never disagrees with what is on disk.

use super::{
    expand_tilde, BoardBlock, ChatConfig, ConfigHandle, DeployBlock, DeployProject, Group,
    OneOrMany,
};
use std::collections::BTreeMap;
use std::io;

/// Which fields of a deploy entry to change; `None` leaves one alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployProjectPatch {
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub target_branch: Option<String>,
}

impl ConfigHandle {
    /// Replaces the whole chat block, as the settings dialog does.
    pub fn save_chat_config(&mut self, chat: ChatConfig) -> io::Result<()> {
        self.config.chat = chat;
        self.save()
    }

    /// Flip whether a group's quiet folders are drawn, and remember it.
    ///
    /// Written back so the choice survives a restart: a toggle you have to set
    /// every launch is one you stop using.
    pub fn toggle_inactive_folders(&mut self) -> io::Result<bool> {
        let next = !self.show_inactive_folders();
        self.config
            .sessions
            .get_or_insert_with(Default::default)
            .show_inactive_folders = Some(next);
        self.save()?;
        Ok(next)
    }

    /// `~` is expanded here, on write, so the stored path is absolute but a
    /// hand-written `~/dev` in the file still works.
    pub fn add_group(&mut self, name: &str, path: &str) -> io::Result<()> {
        let expanded = expand_tilde(path, &self.home);
        self.config.groups.push(Group::new(name, expanded));
        self.save()
    }

    pub fn remove_group(&mut self, index: usize) -> io::Result<()> {
        if index >= self.config.groups.len() {
            return Ok(());
        }
        self.config.groups.remove(index);
        self.save()
    }

    pub fn save_session_nickname(
        &mut self,
        session_id: &str,
        nickname: Option<&str>,
    ) -> io::Result<()> {
        match nickname.filter(|n| !n.is_empty()) {
            Some(nickname) => self
                .config
                .nicknames
                .insert(session_id.to_string(), nickname.to_string()),
            None => self.config.nicknames.remove(session_id),
        };
        self.save()
    }

    /// An empty branch clears the override.
    pub fn set_target_branch(&mut self, project: &str, branch: &str) -> io::Result<()> {
        remove_ci(&mut self.config.target_branches, project);
        let branch = branch.trim();
        if !branch.is_empty() {
            self.config
                .target_branches
                .insert(project.to_string(), branch.to_string());
        }
        self.save()
    }

    /// Replaces a project's repo folders with a single one.
    pub fn set_odoo_project_dir(&mut self, project: &str, dir: &str) -> io::Result<()> {
        let key = ci_key(&self.config.odoo_project_dirs, project);
        self.config
            .odoo_project_dirs
            .insert(key, OneOrMany::One(dir.to_string()));
        self.save()
    }

    pub fn add_odoo_project_dir(&mut self, project: &str, dir: &str) -> io::Result<()> {
        let mut dirs = self.odoo_project_dir_list(project).to_vec();
        if !dirs.iter().any(|d| d == dir) {
            dirs.push(dir.to_string());
        }
        let key = ci_key(&self.config.odoo_project_dirs, project);
        self.config
            .odoo_project_dirs
            .insert(key, OneOrMany::Many(dirs));
        self.save()
    }

    /// Removing the last folder drops the project entry entirely.
    pub fn remove_odoo_project_dir(&mut self, project: &str, dir: &str) -> io::Result<()> {
        let dirs: Vec<String> = self
            .odoo_project_dir_list(project)
            .iter()
            .filter(|d| *d != dir)
            .cloned()
            .collect();
        let key = ci_key(&self.config.odoo_project_dirs, project);
        if dirs.is_empty() {
            self.config.odoo_project_dirs.remove(&key);
        } else {
            self.config
                .odoo_project_dirs
                .insert(key, OneOrMany::Many(dirs));
        }
        self.save()
    }

    /// `None` leaves that half of the filter alone.
    pub fn set_board_project_filter(
        &mut self,
        include: Option<Vec<String>>,
        ignore: Option<Vec<String>>,
    ) -> io::Result<()> {
        let board = self.config.board.get_or_insert_with(BoardBlock::default);
        if let Some(include) = include {
            board.include = include;
        }
        if let Some(ignore) = ignore {
            board.ignore = ignore;
        }
        self.save()
    }

    /// Merges the given fields into a project's deploy entry, promoting a bare
    /// command string to a settings object on the way.
    pub fn set_deploy_project_config(
        &mut self,
        project: &str,
        patch: DeployProjectPatch,
    ) -> io::Result<()> {
        let deploy = self.config.deploy.get_or_insert_with(DeployBlock::default);
        let key = ci_key(&deploy.projects, project);
        let mut settings = deploy
            .projects
            .get(&key)
            .map(DeployProject::settings)
            .unwrap_or_default();
        if let Some(command) = patch.command {
            settings.command = Some(command);
        }
        if let Some(cwd) = patch.cwd {
            settings.cwd = Some(cwd);
        }
        if let Some(target_branch) = patch.target_branch {
            settings.target_branch = Some(target_branch);
        }
        deploy
            .projects
            .insert(key, DeployProject::Settings(settings));
        self.save()
    }

    pub fn remove_deploy_project(&mut self, project: &str) -> io::Result<()> {
        let Some(deploy) = self.config.deploy.as_mut() else {
            return Ok(());
        };
        if remove_ci(&mut deploy.projects, project) {
            return self.save();
        }
        Ok(())
    }
}

/// The key to write under: the one already there, however it is capitalised, or
/// the name as given.
fn ci_key<V>(map: &BTreeMap<String, V>, key: &str) -> String {
    super::lookup_ci(map, key)
        .map(|(stored, _)| stored.to_string())
        .unwrap_or_else(|| key.to_string())
}

/// Removes every case-insensitive match, so a rename cannot leave a duplicate
/// behind. Returns whether anything went.
fn remove_ci<V>(map: &mut BTreeMap<String, V>, key: &str) -> bool {
    let wanted = key.to_lowercase();
    let doomed: Vec<String> = map
        .keys()
        .filter(|stored| stored.to_lowercase() == wanted)
        .cloned()
        .collect();
    let removed = !doomed.is_empty();
    for key in doomed {
        map.remove(&key);
    }
    removed
}
