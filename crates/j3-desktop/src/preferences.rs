//! The loaded `desktop.json` as an entity: remote environments and UI
//! preferences. Every change is written back at once; views observe the
//! entity to follow the preferences.

use std::path::PathBuf;

use gpui_kit::{Context, SharedString};

use crate::{
    environments::{DesktopConfig, RemoteEnvironment},
    ui_settings::UiSettings,
};

pub struct Preferences {
    config: DesktopConfig,
    /// `None` when the system has no config directory.
    path: Option<PathBuf>,
    /// Why the file could not be read or the last change not saved.
    error: Option<SharedString>,
}

impl Preferences {
    /// Loads `path` (creating it with the defaults for `hostname`).
    pub fn load(path: Option<PathBuf>, hostname: &str) -> Self {
        let empty = || DesktopConfig {
            environments: Vec::new(),
            ui: UiSettings::default(),
        };
        let (config, error) = match &path {
            Some(path) => match DesktopConfig::load_or_create(path, hostname) {
                Ok(config) => (config, None),
                Err(e) => (empty(), Some(e.to_string())),
            },
            None => (
                empty(),
                Some(
                    "No config directory; settings and remote environments can’t be saved.".into(),
                ),
            ),
        };
        Self {
            config,
            path,
            error: error.map(Into::into),
        }
    }

    pub fn ui(&self) -> &UiSettings {
        &self.config.ui
    }

    pub fn remotes(&self) -> &[RemoteEnvironment] {
        &self.config.environments
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Changes the UI preferences and saves them. A failed save keeps the
    /// change for this session and reports the error.
    pub fn update_ui(&mut self, change: impl FnOnce(&mut UiSettings), cx: &mut Context<Self>) {
        let mut ui = self.config.ui.clone();
        change(&mut ui);
        if ui == self.config.ui {
            return;
        }
        self.config.ui = ui;
        self.error = self
            .save(&self.config)
            .err()
            .map(|e| format!("Couldn’t save settings: {e}").into());
        cx.notify();
    }

    /// Validates, stores and saves a new remote environment.
    pub fn add_remote(
        &mut self,
        label: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) -> Result<RemoteEnvironment, String> {
        let mut config = self.config.clone();
        let remote = config.add(label, url).map_err(|e| e.to_string())?;
        self.save(&config)?;
        self.config = config;
        cx.notify();
        Ok(remote)
    }

    /// Forgets the remote environment with this URL and saves.
    pub fn remove_remote(&mut self, url: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let mut config = self.config.clone();
        config.remove(url);
        self.save(&config)
            .map_err(|e| format!("Couldn’t save the environments: {e}"))?;
        self.config = config;
        cx.notify();
        Ok(())
    }

    fn save(&self, config: &DesktopConfig) -> Result<(), String> {
        match &self.path {
            Some(path) => config.save(path).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}
