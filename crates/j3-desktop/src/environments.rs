//! Remote environments the desktop connects to, persisted in
//! `<config dir>/j3/desktop.json`. The local environment (the in-process
//! server) is implicit and never stored.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use j3_core::serve::DEFAULT_PORT;

/// One remote j3 server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEnvironment {
    pub label: String,
    pub url: String,
}

/// The contents of `desktop.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopConfig {
    pub environments: Vec<RemoteEnvironment>,
}

impl Default for DesktopConfig {
    /// shieldy, the always-on agent host.
    fn default() -> Self {
        Self {
            environments: vec![RemoteEnvironment {
                label: "shieldy".to_owned(),
                url: format!("http://shieldy:{DEFAULT_PORT}"),
            }],
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
    /// Reads `path`, creating it with [`DesktopConfig::default`] when missing.
    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let config = Self::default();
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
        assert_eq!(normalize_url(" shieldy:4000 ").unwrap(), "http://shieldy:4000");
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
        let mut config = DesktopConfig::default();
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
        assert_eq!(config, DesktopConfig::default());
    }

    #[test]
    fn load_creates_the_default_file_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("j3-desktop-config-{}", std::process::id()));
        let path = dir.join("nested").join("desktop.json");
        let _ = fs::remove_dir_all(&dir);

        let created = DesktopConfig::load_or_create(&path).unwrap();
        assert_eq!(created, DesktopConfig::default());
        let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            serde_json::json!({"environments":[{"label":"shieldy","url":"http://shieldy:3780"}]})
        );

        let mut changed = created;
        changed.add("revived", "revived").unwrap();
        changed.save(&path).unwrap();
        assert_eq!(DesktopConfig::load_or_create(&path).unwrap(), changed);

        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            DesktopConfig::load_or_create(&path),
            Err(ConfigError::Parse { .. })
        ));
        fs::remove_dir_all(&dir).unwrap();
    }
}
