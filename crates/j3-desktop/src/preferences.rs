//! The loaded `desktop.json` (spec §2.5): remote environments and UI preferences. Every
//! change is written back at once; views read `s.prefs` and re-run after the change.

use std::path::PathBuf;

use rune::Tx;

use crate::{
    app::J3,
    environments::{DesktopConfig, RemoteEnvironment},
    ui_settings::UiSettings,
};

pub struct Preferences {
    config: DesktopConfig,
    /// `None` when the system has no config directory.
    path: Option<PathBuf>,
    /// Why the file could not be read or the last change not saved.
    error: Option<String>,
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
            error,
        }
    }

    /// Preferences held in memory only (tests, headless fixtures): nothing is read or saved.
    pub fn in_memory(config: DesktopConfig) -> Self {
        Self {
            config,
            path: None,
            error: None,
        }
    }

    pub fn ui(&self) -> &UiSettings {
        &self.config.ui
    }

    pub fn remotes(&self) -> &[RemoteEnvironment] {
        &self.config.environments
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Changes the UI preferences and saves them; true when anything changed. A failed save
    /// keeps the change for this session and reports the error.
    pub fn update_ui(&mut self, change: impl FnOnce(&mut UiSettings)) -> bool {
        let mut ui = self.config.ui.clone();
        change(&mut ui);
        if ui == self.config.ui {
            return false;
        }
        self.config.ui = ui;
        self.error = self
            .save(&self.config)
            .err()
            .map(|e| format!("Couldn’t save settings: {e}"));
        true
    }

    /// Validates, stores and saves a new remote environment.
    pub fn add_remote(&mut self, label: &str, url: &str) -> Result<RemoteEnvironment, String> {
        let mut config = self.config.clone();
        let remote = config.add(label, url).map_err(|e| e.to_string())?;
        self.save(&config)?;
        self.config = config;
        Ok(remote)
    }

    /// Forgets the remote environment with this URL and saves.
    pub fn remove_remote(&mut self, url: &str) -> Result<(), String> {
        let mut config = self.config.clone();
        config.remove(url);
        self.save(&config)
            .map_err(|e| format!("Couldn’t save the environments: {e}"))?;
        self.config = config;
        Ok(())
    }

    fn save(&self, config: &DesktopConfig) -> Result<(), String> {
        match &self.path {
            Some(path) => config.save(path).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

impl J3 {
    /// Changes the UI preferences, saves them, and applies what the window needs at once
    /// (font families). Every view re-runs after a handler, so sizes, zoom and workflow
    /// follow by themselves.
    pub fn update_ui(&mut self, change: impl FnOnce(&mut UiSettings), tx: &mut Tx) {
        let fonts = crate::theme::families(self.prefs.ui());
        if self.prefs.update_ui(change) && crate::theme::families(self.prefs.ui()) != fonts {
            crate::theme::apply_families(self.prefs.ui(), tx);
        }
    }

    /// Zooms one level in (`steps` > 0) or out (< 0), or back to 100% (0), and saves it.
    pub fn zoom(&mut self, steps: i32, tx: &mut Tx) {
        self.update_ui(
            |ui| ui.zoom = if steps == 0 { 1.0 } else { ui.zoom_step(steps) },
            tx,
        );
    }
}
