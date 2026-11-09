//! Environments (spec §2.4): one j3 server each, its connection, the latest connection
//! state forwarded from its `watch` channel, its model catalog, the project slash commands
//! fetched through it and the chat images fetched through it. [`Environments`] keeps them in
//! order, the local environment first, and [`Environments::distinct`] hides servers reached
//! twice.

use std::{collections::BTreeMap, sync::Arc};

use base64::Engine as _;
use j3_client::{
    Connection, ConnectionState, Status,
    entities::Attachment,
    methods::{CatalogCommand, ImageUrlParams},
};
use rune::Tx;

use crate::{
    app::J3,
    images::ImageLoad,
    inbox::Inbox,
    local_server::LocalServerStatus,
    models::{self, ModelChoice},
    services::Services,
};

/// Key of the local environment.
pub const LOCAL_KEY: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentKind {
    /// The in-process server (or the j3-server already on its port).
    Local(LocalServerStatus),
    Remote,
}

/// An image the chat shows, by where its bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageRef {
    /// An attachment on the server: an uploaded image or a tool's output image.
    Attachment { id: String },
    /// An image file in a project on the thread's host, relative to the project.
    HostFile { project_id: String, path: String },
    /// An `http(s):` or `data:` URL, read by this app itself.
    Url(String),
}

impl ImageRef {
    /// The image of an attachment; None for other files.
    pub fn attachment(attachment: &Attachment) -> Option<Self> {
        attachment
            .mime_type
            .starts_with("image/")
            .then(|| Self::Attachment {
                id: attachment.id.clone(),
            })
    }

    /// The image a markdown `![](url)` in a thread of project `project_id` names: web and
    /// `data:` URLs as they are, anything else a (percent-encoded) path in the project, with
    /// or without `file://`.
    pub fn markdown(url: &str, project_id: &str) -> Self {
        let url = url.trim();
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase());
        match scheme.as_deref() {
            Some("http" | "https" | "data") => Self::Url(url.to_owned()),
            _ => {
                let path = url.strip_prefix("file://").unwrap_or(url);
                let path = path.split(['?', '#']).next().unwrap_or_default();
                Self::HostFile {
                    project_id: project_id.to_owned(),
                    path: String::from_utf8_lossy(&percent_decode(path)).into_owned(),
                }
            }
        }
    }
}

pub struct Environment {
    /// Stable identity: `local`, or the remote base URL.
    key: String,
    label: String,
    kind: EnvironmentKind,
    connection: Connection,
    state: Arc<ConnectionState>,
    /// The labelled models `state`'s catalog offers, rebuilt when they change.
    models: Arc<[ModelChoice]>,
    /// Each project's own slash commands, by project id, as `server.refresh` last returned
    /// them (the catalog the server streams has none).
    project_commands: BTreeMap<String, Vec<CatalogCommand>>,
    /// The task forwarding `connection.state()` into the inbox; stopped on drop.
    watch: Option<tokio::task::AbortHandle>,
}

impl Environment {
    /// Opens a connection to `url` and forwards its state changes into `inbox` as
    /// `J3::environment_changed` updates.
    pub fn open(
        key: impl Into<String>,
        label: impl Into<String>,
        url: &str,
        kind: EnvironmentKind,
        services: &Services,
        inbox: &Inbox,
    ) -> Self {
        let key = key.into();
        let connection = Connection::open(url, services.runtime.clone());
        services.register(&connection);
        let mut states = connection.state();
        let state = states.borrow_and_update().clone();
        let inbox = inbox.clone();
        let env = key.clone();
        let watch = services.runtime.spawn(async move {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                let env = env.clone();
                inbox.post(move |s, tx| s.environment_changed(&env, state, tx));
            }
        });
        Self::with_state(
            key,
            label,
            kind,
            connection,
            state,
            Some(watch.abort_handle()),
        )
    }

    /// An environment over `connection` whose state the caller sets (tests, fixtures).
    pub fn with_state(
        key: impl Into<String>,
        label: impl Into<String>,
        kind: EnvironmentKind,
        connection: Connection,
        state: Arc<ConnectionState>,
        watch: Option<tokio::task::AbortHandle>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind,
            connection,
            models: model_catalog(&state),
            state,
            project_commands: BTreeMap::new(),
            watch,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn set_label(&mut self, label: String) {
        self.label = label;
    }

    pub fn kind(&self) -> &EnvironmentKind {
        &self.kind
    }

    pub fn is_local(&self) -> bool {
        matches!(self.kind, EnvironmentKind::Local(_))
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn state(&self) -> &Arc<ConnectionState> {
        &self.state
    }

    /// The models this server offers, labelled for display.
    pub fn models(&self) -> &Arc<[ModelChoice]> {
        &self.models
    }

    /// The server's stable id, once its info is known.
    pub fn server_id(&self) -> Option<&str> {
        self.state.info.as_ref().map(|info| info.server_id.as_str())
    }

    /// Connected and caught up with the server's shell.
    pub fn is_live(&self) -> bool {
        matches!(self.state.status, Status::Connected) && self.state.shell.synced
    }

    /// Whether the server offers capability `name` (`git`, `terminal`, …); true until its
    /// info is known.
    pub fn has_capability(&self, name: &str) -> bool {
        self.state
            .info
            .as_ref()
            .is_none_or(|info| info.capabilities.iter().any(|c| c == name))
    }

    /// A short status for the sidebar when not live (`None` when live).
    pub fn status_label(&self) -> Option<&'static str> {
        match &self.state.status {
            Status::Connected if self.state.shell.synced => None,
            Status::Connecting | Status::Connected => Some("Connecting…"),
            Status::Reconnecting { .. } => Some("Offline"),
            Status::Incompatible { .. } => Some("Incompatible"),
            Status::Closed => Some("Closed"),
        }
    }

    /// Everything known about the connection, for a tooltip.
    pub fn status_detail(&self) -> String {
        let mut lines = vec![self.connection.http_base().to_owned()];
        if let EnvironmentKind::Local(status) = &self.kind {
            lines.push(status.summary());
        }
        lines.push(match &self.state.status {
            Status::Connecting => "Connecting".to_owned(),
            Status::Connected if !self.state.shell.synced => "Synchronizing".to_owned(),
            Status::Connected => "Connected".to_owned(),
            Status::Reconnecting { attempt, error } => {
                format!("Reconnecting (attempt {attempt}): {error}")
            }
            Status::Incompatible { message } => format!("Incompatible: {message}"),
            Status::Closed => "Closed".to_owned(),
        });
        if let Some(info) = &self.state.info {
            lines.push(format!("{} · j3 {}", info.name, info.version));
        }
        lines.join("\n")
    }

    /// Stores a newer state, rebuilding the model catalog when the models changed.
    pub fn set_state(&mut self, state: Arc<ConnectionState>) {
        let old = self.state.catalog.as_ref().map(|catalog| &catalog.models);
        if old != state.catalog.as_ref().map(|catalog| &catalog.models) {
            self.models = model_catalog(&state);
        }
        self.state = state;
    }

    /// Project `project_id`'s own slash commands, once fetched (`server.refresh`).
    pub fn project_commands(&self, project_id: &str) -> Option<&[CatalogCommand]> {
        self.project_commands.get(project_id).map(Vec::as_slice)
    }

    /// Stores project slash commands `server.refresh` returned.
    pub fn set_project_commands(&mut self, commands: BTreeMap<String, Vec<CatalogCommand>>) {
        self.project_commands.extend(commands);
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        if let Some(watch) = self.watch.take() {
            watch.abort();
        }
    }
}

/// The labelled model catalog of `state`'s catalog; empty without one.
fn model_catalog(state: &ConnectionState) -> Arc<[ModelChoice]> {
    state
        .catalog
        .as_ref()
        .map(models::model_choices)
        .unwrap_or_default()
        .into()
}

/// The environments in order: the local one first, then the remotes of `desktop.json`.
#[derive(Default)]
pub struct Environments {
    list: Vec<Environment>,
}

impl Environments {
    pub fn iter(&self) -> impl Iterator<Item = &Environment> {
        self.list.iter()
    }

    pub fn get(&self, key: &str) -> Option<&Environment> {
        self.list.iter().find(|env| env.key == key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Environment> {
        self.list.iter_mut().find(|env| env.key == key)
    }

    /// The local environment (always first once opened).
    pub fn local(&self) -> Option<&Environment> {
        self.list.first().filter(|env| env.is_local())
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn push(&mut self, env: Environment) {
        self.list.push(env);
    }

    pub fn remove(&mut self, key: &str) -> Option<Environment> {
        let ix = self.list.iter().position(|env| env.key == key)?;
        Some(self.list.remove(ix))
    }

    /// The environments without the ones that reach a server already listed
    /// (e.g. the default `shieldy` remote on shieldy itself). Their config
    /// entries stay; they reappear if the ids ever differ.
    pub fn distinct(&self) -> Vec<&Environment> {
        let ids: Vec<Option<&str>> = self.list.iter().map(Environment::server_id).collect();
        distinct(&ids)
            .into_iter()
            .map(|ix| &self.list[ix])
            .collect()
    }

    /// Every connection, for shutdown and commands across environments.
    pub fn connections(&self) -> Vec<Connection> {
        self.list.iter().map(|env| env.connection.clone()).collect()
    }
}

/// Indices of the entries to show, given each one's server id (`None` until its info
/// arrives): an entry whose id an earlier entry already has is the same server reached twice
/// and is left out. The local environment is listed first, so it wins.
pub fn distinct(ids: &[Option<&str>]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .enumerate()
        .filter(|(_, id)| id.is_none_or(|id| seen.insert(id)))
        .map(|(ix, _)| ix)
        .collect()
}

impl J3 {
    /// Opens the local environment and every remote of `desktop.json`.
    pub fn open_environments(&mut self) {
        let name = crate::device::name(self.prefs.ui().device_name.as_deref());
        let status = match &self.services.local {
            Some(local) => local.status().clone(),
            None => LocalServerStatus::External("headless run; no server started".into()),
        };
        let local = Environment::open(
            LOCAL_KEY,
            name,
            self.services.local_url(),
            EnvironmentKind::Local(status),
            &self.services,
            &self.inbox,
        );
        self.envs.push(local);
        for remote in self.prefs.remotes().to_vec() {
            let env = Environment::open(
                remote.url.clone(),
                remote.label,
                &remote.url,
                EnvironmentKind::Remote,
                &self.services,
                &self.inbox,
            );
            self.envs.push(env);
        }
    }

    /// Names this computer `name` (blank: inferred from the machine again), saves it, and
    /// relabels the local environment. The local server reports it to other devices from
    /// its next start.
    pub fn set_device_name(&mut self, name: &str, tx: &mut Tx) {
        let name = name.trim();
        let name = (!name.is_empty()).then(|| name.to_owned());
        self.update_ui(|ui| ui.device_name = name, tx);
        let label = crate::device::name(self.prefs.ui().device_name.as_deref());
        if let Some(local) = self.envs.get_mut(LOCAL_KEY) {
            local.set_label(label);
        }
    }

    /// Validates, saves and connects a new remote environment.
    pub fn add_environment(&mut self, label: &str, url: &str) -> Result<(), String> {
        let remote = self.prefs.add_remote(label, url)?;
        let env = Environment::open(
            remote.url.clone(),
            remote.label,
            &remote.url,
            EnvironmentKind::Remote,
            &self.services,
            &self.inbox,
        );
        self.envs.push(env);
        Ok(())
    }

    /// Forgets a remote environment: saves `desktop.json`, leaves anything of it on screen,
    /// and closes its connection.
    pub fn remove_environment(&mut self, key: &str, tx: &mut Tx) -> Result<(), String> {
        self.prefs.remove_remote(key)?;
        let Some(env) = self.envs.remove(key) else {
            return Ok(());
        };
        self.forget_environment(key, tx);
        self.images.forget_env(key);
        self.services.unregister(env.connection());
        let connection = env.connection().clone();
        self.services
            .runtime
            .spawn(async move { connection.close().await });
        Ok(())
    }

    /// A newer state for environment `key`, from its watch-forwarding task.
    pub fn environment_changed(&mut self, key: &str, state: Arc<ConnectionState>, tx: &mut Tx) {
        let Some(env) = self.envs.get_mut(key) else {
            return;
        };
        env.set_state(state);
        self.observe_attention(key, tx);
        self.prune_composers(tx);
    }

    /// The image `image` of environment `env` for a view: the cached load, else `Loading`
    /// while a fetch is recorded for the next `poll`.
    pub fn image(&self, env: &str, image: &ImageRef) -> ImageLoad {
        let key = (env.to_owned(), image.clone());
        match self.images.get(&key) {
            Some(load) => load,
            None => {
                self.inbox.wake();
                ImageLoad::Loading
            }
        }
    }

    /// Starts the fetches views recorded since the last poll; true when any started.
    pub(crate) fn fetch_wanted_images(&mut self) -> bool {
        let wanted = self.images.take_wanted();
        let started = !wanted.is_empty();
        for key in wanted {
            let (env, image) = key.clone();
            let Some(connection) = self.envs.get(&env).map(|env| env.connection().clone()) else {
                self.images.set(key, ImageLoad::Failed);
                continue;
            };
            self.images.set(key.clone(), ImageLoad::Loading);
            self.spawn(async move {
                let bytes = match &image {
                    ImageRef::Url(url) if url.starts_with("data:") => data_url(url),
                    ImageRef::Url(url) => fetch(url).await,
                    ImageRef::Attachment { id } => fetch(&connection.attachment_url(id)).await,
                    ImageRef::HostFile { project_id, path } => {
                        let params = ImageUrlParams {
                            project_id: project_id.clone(),
                            path: path.clone(),
                        };
                        match connection.projects_image_url(params).await {
                            Ok(granted) => fetch(&connection.url(&granted.url)).await,
                            Err(e) => {
                                tracing::debug!("image url for {image:?}: {e}");
                                None
                            }
                        }
                    }
                };
                // An image arriving is not text arriving: rows take its size at once rather
                // than gliding the feed to its end.
                move |s: &mut J3, tx: &mut Tx| {
                    let load = match bytes {
                        Some(bytes) => s.images.decode(bytes),
                        None => ImageLoad::Failed,
                    };
                    s.images.set(key, load);
                    tx.instant();
                }
            });
        }
        started
    }
}

async fn fetch(url: &str) -> Option<Vec<u8>> {
    let response = reqwest::get(url).await.ok()?.error_for_status().ok()?;
    response.bytes().await.ok().map(|bytes| bytes.to_vec())
}

/// The bytes a `data:` URL carries (base64 or percent-encoded).
fn data_url(url: &str) -> Option<Vec<u8>> {
    let (header, payload) = url.strip_prefix("data:")?.split_once(',')?;
    if header.ends_with(";base64") {
        let payload: String = percent_decode(payload)
            .into_iter()
            .filter(|byte| !byte.is_ascii_whitespace())
            .map(char::from)
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()
    } else {
        Some(percent_decode(payload))
    }
}

/// `%XX` escapes decoded; malformed escapes stay as they are.
fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match (
            bytes[ix],
            bytes.get(ix + 1).copied(),
            bytes.get(ix + 2).copied(),
        ) {
            (b'%', Some(high), Some(low)) if hex(high).is_some() && hex(low).is_some() => {
                out.push((hex(high).unwrap_or(0) * 16 + hex(low).unwrap_or(0)) as u8);
                ix += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                ix += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::{Format, sniff};

    #[test]
    fn the_same_server_is_listed_once() {
        // local, shieldy (same server as local), revived, unknown, unknown
        let ids = [Some("env-a"), Some("env-a"), Some("env-b"), None, None];
        assert_eq!(distinct(&ids), [0, 2, 3, 4]);
        assert_eq!(distinct(&[Some("x"), Some("y")]), [0, 1]);
        assert_eq!(distinct(&[]), Vec::<usize>::new());
    }

    #[test]
    fn markdown_images_resolve_to_urls_or_host_files() {
        let host = |path: &str| ImageRef::HostFile {
            project_id: "T".into(),
            path: path.into(),
        };
        assert_eq!(
            ImageRef::markdown("https://x.dev/a.png", "T"),
            ImageRef::Url("https://x.dev/a.png".into())
        );
        assert_eq!(
            ImageRef::markdown("viz/my%20plot.png", "T"),
            host("viz/my plot.png")
        );
        assert_eq!(
            ImageRef::markdown("file:///tmp/shot.png?v=2#x", "T"),
            host("/tmp/shot.png")
        );
        let png = "data:image/png;base64,iVBORw0K\nGgo=";
        assert_eq!(
            ImageRef::markdown(png, "T"),
            ImageRef::Url(png.trim().into())
        );
        let bytes = data_url(png).unwrap();
        assert_eq!(sniff(&bytes), Some(Format::Png));
        let svg = data_url("data:image/svg+xml,%3Csvg%20xmlns='x'/%3E").unwrap();
        assert_eq!(sniff(&svg), Some(Format::Svg));
    }
}
