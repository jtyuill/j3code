//! `<config dir>/j3/desktop.json`: the remote environments the desktop
//! connects to and its UI preferences. The local environment (the
//! in-process server) is implicit and never stored.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use j3_core::serve::DEFAULT_PORT;

use crate::ui_settings::UiSettings;

/// One remote j3 server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEnvironment {
    pub label: String,
    pub url: String,
}

/// The contents of `desktop.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopConfig {
    pub environments: Vec<RemoteEnvironment>,
    /// Absent in files from before the preferences existed.
    #[serde(default)]
    pub ui: UiSettings,
}

/// The always-on agent host every desktop connects to by default.
const DEFAULT_REMOTE: &str = "shieldy";

impl DesktopConfig {
    /// The config a fresh `desktop.json` gets on `hostname`: shieldy as a
    /// remote, except on shieldy itself (its local environment already is
    /// that server).
    pub fn default_for_host(hostname: &str) -> Self {
        let short = hostname.split('.').next().unwrap_or_default();
        let environments = if short.eq_ignore_ascii_case(DEFAULT_REMOTE) {
            Vec::new()
        } else {
            vec![RemoteEnvironment {
                label: DEFAULT_REMOTE.to_owned(),
                url: format!("http://{DEFAULT_REMOTE}:{DEFAULT_PORT}"),
            }]
        };
        Self {
            environments,
            ui: UiSettings::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no config directory on this system")]
    NoConfigDir,
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Why an environment cannot be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddError {
    #[error("Enter a name.")]
    EmptyLabel,
    #[error("Enter a host or URL.")]
    EmptyUrl,
    #[error("Use an http:// or https:// URL.")]
    UnsupportedScheme,
    #[error("“{0}” is already an environment.")]
    DuplicateLabel(String),
    #[error("{0} is already an environment.")]
    DuplicateUrl(String),
}

/// `<config dir>/j3/desktop.json`.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    dirs::config_dir()
        .map(|dir| dir.join("j3").join("desktop.json"))
        .ok_or(ConfigError::NoConfigDir)
}

impl DesktopConfig {
    /// Reads `path`, creating it with [`DesktopConfig::default_for_host`]
    /// when missing. A file that stores the workflow under its old name is
    /// rewritten with the current one.
    pub fn load_or_create(path: &Path, hostname: &str) -> Result<Self, ConfigError> {
        match fs::read(path) {
            Ok(bytes) => {
                let parse = |source| ConfigError::Parse {
                    path: path.to_owned(),
                    source,
                };
                let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(parse)?;
                let stored = raw.pointer("/ui/workflow").cloned();
                let config: Self = serde_json::from_value(raw).map_err(parse)?;
                let current = serde_json::to_value(config.ui.workflow).ok();
                if stored.is_some() && stored != current {
                    config.save(path)?;
                }
                Ok(config)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let config = Self::default_for_host(hostname);
                config.save(path)?;
                Ok(config)
            }
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the config atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io_error = |source| ConfigError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_error)?;
        }
        let mut json = serde_json::to_vec_pretty(self).expect("config always encodes");
        json.push(b'\n');
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(io_error)?;
        fs::rename(&tmp, path).map_err(io_error)
    }

    /// Validates and appends a remote environment; returns it as stored.
    pub fn add(&mut self, label: &str, url: &str) -> Result<RemoteEnvironment, AddError> {
        let label = label.trim();
        if label.is_empty() {
            return Err(AddError::EmptyLabel);
        }
        let url = normalize_url(url)?;
        if let Some(existing) = self
            .environments
            .iter()
            .find(|e| e.label.eq_ignore_ascii_case(label))
        {
            return Err(AddError::DuplicateLabel(existing.label.clone()));
        }
        if self.environments.iter().any(|e| e.url == url) {
            return Err(AddError::DuplicateUrl(url));
        }
        let environment = RemoteEnvironment {
            label: label.to_owned(),
            url,
        };
        self.environments.push(environment.clone());
        Ok(environment)
    }

    /// Removes the environment with this URL; returns whether one was removed.
    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.environments.len();
        self.environments.retain(|e| e.url != url);
        self.environments.len() != before
    }
}

/// Turns user input like `shieldy`, `shieldy:3780` or `https://host/` into a
/// base URL `scheme://host:port` with no trailing slash. A missing scheme
/// means `http`; a missing port means [`DEFAULT_PORT`] for `http`.
pub fn normalize_url(input: &str) -> Result<String, AddError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AddError::EmptyUrl);
    }
    let (scheme, rest) = match input.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("http".to_owned(), input),
    };
    if scheme != "http" && scheme != "https" {
        return Err(AddError::UnsupportedScheme);
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err(AddError::EmptyUrl);
    }
    let has_port = match authority.rsplit_once(':') {
        // `[::1]` alone has colons but no port.
        Some((host, port)) => {
            !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    };
    Ok(if has_port || scheme == "https" {
        format!("{scheme}://{authority}")
    } else {
        format!("{scheme}://{authority}:{DEFAULT_PORT}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_hosts_and_urls() {
        assert_eq!(normalize_url("shieldy").unwrap(), "http://shieldy:3780");
        assert_eq!(
            normalize_url(" shieldy:4000 ").unwrap(),
            "http://shieldy:4000"
        );
        assert_eq!(
            normalize_url("http://100.64.0.7:3780/").unwrap(),
            "http://100.64.0.7:3780"
        );
        assert_eq!(
            normalize_url("HTTP://revived/ws").unwrap(),
            "http://revived:3780"
        );
        assert_eq!(
            normalize_url("https://j3.example.com").unwrap(),
            "https://j3.example.com"
        );
        assert_eq!(normalize_url("[::1]").unwrap(), "http://[::1]:3780");
        assert_eq!(normalize_url("[::1]:9000").unwrap(), "http://[::1]:9000");
        assert_eq!(normalize_url(""), Err(AddError::EmptyUrl));
        assert_eq!(normalize_url("http://"), Err(AddError::EmptyUrl));
        assert_eq!(
            normalize_url("ftp://shieldy"),
            Err(AddError::UnsupportedScheme)
        );
    }

    #[test]
    fn add_rejects_blank_and_duplicate_environments() {
        let mut config = DesktopConfig::default_for_host("revolver.local");
        assert_eq!(config.add(" ", "x"), Err(AddError::EmptyLabel));
        assert_eq!(
            config.add("SHIELDY", "other"),
            Err(AddError::DuplicateLabel("shieldy".to_owned()))
        );
        assert_eq!(
            config.add("box", "shieldy"),
            Err(AddError::DuplicateUrl("http://shieldy:3780".to_owned()))
        );
        let added = config.add(" revived ", "revived").unwrap();
        assert_eq!(
            added,
            RemoteEnvironment {
                label: "revived".to_owned(),
                url: "http://revived:3780".to_owned()
            }
        );
        assert_eq!(config.environments.len(), 2);
        assert!(config.remove("http://revived:3780"));
        assert!(!config.remove("http://revived:3780"));
        assert_eq!(config, DesktopConfig::default_for_host("revolver.local"));
    }

    #[test]
    fn shieldy_does_not_get_itself_as_a_remote() {
        assert_eq!(
            DesktopConfig::default_for_host("revolver")
                .environments
                .len(),
            1
        );
        assert!(
            DesktopConfig::default_for_host("shieldy")
                .environments
                .is_empty()
        );
        assert!(
            DesktopConfig::default_for_host("Shieldy.tail1234.ts.net")
                .environments
                .is_empty()
        );
        assert_eq!(
            DesktopConfig::default_for_host("shieldy2")
                .environments
                .len(),
            1
        );
    }

    #[test]
    fn load_creates_the_default_file_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("j3-desktop-config-{}", std::process::id()));
        let path = dir.join("nested").join("desktop.json");
        let _ = fs::remove_dir_all(&dir);

        let created = DesktopConfig::load_or_create(&path, "revolver").unwrap();
        assert_eq!(created, DesktopConfig::default_for_host("revolver"));
        let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk["environments"],
            serde_json::json!([{"label":"shieldy","url":"http://shieldy:3780"}])
        );
        assert_eq!(on_disk["ui"]["workflow"], "inbox");

        let mut changed = created;
        changed.add("revived", "revived").unwrap();
        changed.save(&path).unwrap();
        assert_eq!(
            DesktopConfig::load_or_create(&path, "revolver").unwrap(),
            changed
        );

        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            DesktopConfig::load_or_create(&path, "revolver"),
            Err(ConfigError::Parse { .. })
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_old_workflow_name_is_rewritten_on_load() {
        let dir = std::env::temp_dir().join(format!("j3-desktop-migrate-{}", std::process::id()));
        let path = dir.join("desktop.json");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = r#"{"environments": [], "ui": {"workflow": "t3"}}"#; // deny-list: allow
        fs::write(&path, old).unwrap();

        let config = DesktopConfig::load_or_create(&path, "revolver").unwrap();
        assert_eq!(config.ui.workflow, crate::ui_settings::Workflow::Inbox);
        let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk["ui"]["workflow"], "inbox");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_without_ui_preferences_load_with_defaults() {
        let config: DesktopConfig =
            serde_json::from_value(serde_json::json!({"environments": []})).unwrap();
        assert_eq!(config.ui, UiSettings::default());
    }
}
