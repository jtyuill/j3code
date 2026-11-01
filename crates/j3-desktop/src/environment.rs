//! One environment (j3 server) as a GPUI entity: its connection, the latest
//! connection state, and attachment images fetched from it.

use std::{collections::HashMap, sync::Arc};

use gpui_kit::{App, Context, Entity, Image, ImageFormat, SharedString, Task};
use j3_client::{
    Connection, ConnectionState, ConnectionStatus, t3::chat_attachment::ChatAttachment,
};

use crate::{local_server::LocalServerStatus, services::Services};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentKind {
    /// The in-process server (or the j3-server already on its port).
    Local(LocalServerStatus),
    Remote,
}

/// An attachment image being fetched for display.
#[derive(Clone)]
pub enum ImageLoad {
    Loading,
    Loaded(Arc<Image>),
    Failed,
}

pub struct Environment {
    /// Stable identity: `local`, or the remote base URL.
    key: SharedString,
    label: SharedString,
    kind: EnvironmentKind,
    connection: Connection,
    state: Arc<ConnectionState>,
    images: HashMap<String, ImageLoad>,
    _watch: Task<()>,
}

/// Key of the local environment.
pub const LOCAL_KEY: &str = "local";

impl Environment {
    /// The server's stable `environmentId`, once its descriptor is known.
    pub fn environment_id(&self) -> Option<&str> {
        self.state
            .descriptor
            .as_ref()
            .map(|descriptor| descriptor.environment_id.as_str())
    }
}

impl Environment {
    pub fn new(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        url: &str,
        kind: EnvironmentKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let connection = Connection::open(url, Services::global(cx).runtime().clone());
        let mut states = connection.state();
        let state = states.borrow_and_update().clone();
        let watch = cx.spawn(async move |this, cx| {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                let updated = this.update(cx, |this, cx| {
                    this.state = state;
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        Self {
            key: key.into(),
            label: label.into(),
            kind,
            connection,
            state,
            images: HashMap::new(),
            _watch: watch,
        }
    }

    pub fn key(&self) -> &SharedString {
        &self.key
    }

    pub fn label(&self) -> &SharedString {
        &self.label
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

    pub fn is_live(&self) -> bool {
        matches!(self.state.status, ConnectionStatus::Live)
    }

    /// A short status for the sidebar when not live (`None` when live).
    pub fn status_label(&self) -> Option<&'static str> {
        match &self.state.status {
            ConnectionStatus::Live => None,
            ConnectionStatus::Connecting | ConnectionStatus::Synchronizing => Some("Connecting…"),
            ConnectionStatus::Reconnecting { .. } => Some("Offline"),
            ConnectionStatus::Closed => Some("Closed"),
        }
    }

    /// Everything known about the connection, for a tooltip.
    pub fn status_detail(&self) -> String {
        let mut lines = vec![self.connection.http_base().to_owned()];
        if let EnvironmentKind::Local(status) = &self.kind {
            lines.push(status.summary());
        }
        lines.push(match &self.state.status {
            ConnectionStatus::Connecting => "Connecting".to_owned(),
            ConnectionStatus::Synchronizing => "Synchronizing".to_owned(),
            ConnectionStatus::Live => "Connected".to_owned(),
            ConnectionStatus::Reconnecting { attempt, error } => {
                format!("Reconnecting (attempt {attempt}): {error}")
            }
            ConnectionStatus::Closed => "Closed".to_owned(),
        });
        if let Some(descriptor) = &self.state.descriptor {
            lines.push(format!(
                "{} · j3 {}",
                descriptor.label, descriptor.server_version
            ));
        }
        lines.join("\n")
    }

    /// The image of an attachment, fetching it on first use. Notifies when a
    /// fetch finishes.
    pub fn image(&mut self, attachment: &ChatAttachment, cx: &mut Context<Self>) -> ImageLoad {
        let (id, mime_type) = match attachment {
            ChatAttachment::Image(image) => (image.id.clone(), image.mime_type.clone()),
            _ => return ImageLoad::Failed,
        };
        if let Some(load) = self.images.get(&id) {
            return load.clone();
        }
        self.images.insert(id.clone(), ImageLoad::Loading);
        let connection = self.connection.clone();
        let attachment = attachment.clone();
        let services = Services::global(cx);
        let runtime = services.runtime().clone();
        cx.spawn(async move |this, cx| {
            let bytes = match connection.asset_url(&attachment).await {
                Ok(url) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    runtime.spawn(async move {
                        let _ = tx.send(fetch(&url).await);
                    });
                    rx.await.ok().flatten()
                }
                Err(e) => {
                    tracing::warn!("asset url for {id}: {e}");
                    None
                }
            };
            let load = match (bytes, image_format(&mime_type)) {
                (Some(bytes), Some(format)) => {
                    ImageLoad::Loaded(Arc::new(Image::from_bytes(format, bytes)))
                }
                _ => ImageLoad::Failed,
            };
            let _ = this.update(cx, |this, cx| {
                this.images.insert(id, load);
                cx.notify();
            });
        })
        .detach();
        ImageLoad::Loading
    }
}

/// Indices of the entries to show, given each one's server environment id
/// (`None` until its descriptor arrives): an entry whose id an earlier entry
/// already has is the same server reached twice and is left out. The local
/// environment is listed first, so it wins.
pub fn distinct(ids: &[Option<&str>]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .enumerate()
        .filter(|(_, id)| id.is_none_or(|id| seen.insert(id)))
        .map(|(ix, _)| ix)
        .collect()
}

/// `environments` without the ones that reach a server already listed
/// (e.g. the default `shieldy` remote on shieldy itself). Their config
/// entries stay; they reappear if the ids ever differ.
pub fn distinct_environments(
    environments: &[Entity<Environment>],
    cx: &App,
) -> Vec<Entity<Environment>> {
    let ids: Vec<Option<&str>> = environments
        .iter()
        .map(|env| env.read(cx).environment_id())
        .collect();
    distinct(&ids)
        .into_iter()
        .map(|ix| environments[ix].clone())
        .collect()
}

async fn fetch(url: &str) -> Option<Vec<u8>> {
    let response = reqwest::get(url).await.ok()?.error_for_status().ok()?;
    response.bytes().await.ok().map(|bytes| bytes.to_vec())
}

/// The GPUI image format for an image MIME type.
pub fn image_format(mime_type: &str) -> Option<ImageFormat> {
    Some(match mime_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_server_is_listed_once() {
        // local, shieldy (same server as local), revived, unknown, unknown
        let ids = [Some("env-a"), Some("env-a"), Some("env-b"), None, None];
        assert_eq!(distinct(&ids), [0, 2, 3, 4]);
        assert_eq!(distinct(&[Some("x"), Some("y")]), [0, 1]);
        assert_eq!(distinct(&[]), Vec::<usize>::new());
    }
}
