//! One environment (j3 server) as a GPUI entity: its connection, the latest
//! connection state, and the chat images fetched through it.

use std::{collections::HashMap, sync::Arc};

use base64::Engine as _;
use gpui_kit::{App, Context, Entity, Image, ImageFormat, SharedString, Task};
use j3_client::{
    Connection, ConnectionState, ConnectionStatus,
    t3::{
        assets::{AssetDisposition, AssetResource},
        chat_attachment::ChatAttachment,
    },
};

use crate::{
    local_server::LocalServerStatus,
    models::{self, ModelChoice},
    services::Services,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentKind {
    /// The in-process server (or the j3-server already on its port).
    Local(LocalServerStatus),
    Remote,
}

/// An image being fetched for display.
#[derive(Clone)]
pub enum ImageLoad {
    Loading,
    Loaded(Arc<Image>),
    Failed,
}

/// An image the chat shows, by where its bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageRef {
    /// An uploaded image attachment.
    Attachment {
        id: String,
        name: String,
        mime_type: String,
    },
    /// Image `index` of a tool call's output.
    ToolOutput {
        thread_id: String,
        item_id: String,
        index: u64,
    },
    /// An image file on the thread's host: relative to its project, or
    /// absolute.
    HostFile { thread_id: String, path: String },
    /// An `http(s):` or `data:` URL, read by this app itself.
    Url(String),
}

impl ImageRef {
    /// The image of an attachment; None for other files.
    pub fn attachment(attachment: &ChatAttachment) -> Option<Self> {
        match attachment {
            ChatAttachment::Image(image) => Some(Self::Attachment {
                id: image.id.clone(),
                name: image.name.clone(),
                mime_type: image.mime_type.clone(),
            }),
            _ => None,
        }
    }

    /// The image a markdown `![](url)` in thread `thread_id` names: web and
    /// `data:` URLs as they are, anything else a (percent-encoded) path on
    /// the thread's host, with or without `file://`.
    pub fn markdown(url: &str, thread_id: &str) -> Self {
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
                    thread_id: thread_id.to_owned(),
                    path: String::from_utf8_lossy(&percent_decode(path)).into_owned(),
                }
            }
        }
    }

    /// The server resource holding the image; None for URLs.
    fn resource(&self) -> Option<AssetResource> {
        Some(match self {
            Self::Attachment {
                id,
                name,
                mime_type,
            } => AssetResource::Attachment {
                attachment_id: id.clone(),
                file_name: Some(name.clone()),
                mime_type: Some(mime_type.clone()),
                disposition: Some(AssetDisposition::Inline),
                render_intent: None,
            },
            Self::ToolOutput {
                thread_id,
                item_id,
                index,
            } => AssetResource::ToolOutputImage {
                thread_id: thread_id.clone(),
                item_id: item_id.clone(),
                index: *index,
            },
            Self::HostFile { thread_id, path } => AssetResource::WorkspaceFile {
                thread_id: thread_id.clone(),
                path: path.clone(),
            },
            Self::Url(_) => return None,
        })
    }
}

pub struct Environment {
    /// Stable identity: `local`, or the remote base URL.
    key: SharedString,
    label: SharedString,
    kind: EnvironmentKind,
    connection: Connection,
    state: Arc<ConnectionState>,
    /// The labelled models `state`'s config offers, rebuilt when its
    /// providers change.
    models: Arc<[ModelChoice]>,
    images: HashMap<ImageRef, ImageLoad>,
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
                    let old = this.state.config.as_ref().map(|config| &config.providers);
                    if old != state.config.as_ref().map(|config| &config.providers) {
                        this.models = model_catalog(&state);
                    }
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
            models: model_catalog(&state),
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

    /// The models this server offers, labelled for display.
    pub fn models(&self) -> &Arc<[ModelChoice]> {
        &self.models
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

    /// An image for the chat, fetching it on first use. Notifies when a
    /// fetch finishes.
    pub fn image(&mut self, image: &ImageRef, cx: &mut Context<Self>) -> ImageLoad {
        if let Some(load) = self.images.get(image) {
            return load.clone();
        }
        self.images.insert(image.clone(), ImageLoad::Loading);
        let connection = self.connection.clone();
        let image = image.clone();
        let runtime = Services::global(cx).runtime().clone();
        cx.spawn(async move |this, cx| {
            let bytes = match (&image, image.resource()) {
                (ImageRef::Url(url), _) if url.starts_with("data:") => data_url(url),
                (ImageRef::Url(url), _) => fetch_on(&runtime, url.clone()).await,
                (_, Some(resource)) => match connection.resource_url(resource).await {
                    Ok(url) => fetch_on(&runtime, url).await,
                    Err(e) => {
                        tracing::debug!("asset url for {image:?}: {e}");
                        None
                    }
                },
                (_, None) => None,
            };
            let load = match bytes.and_then(|bytes| Some((sniff_format(&bytes)?, bytes))) {
                Some((format, bytes)) => {
                    ImageLoad::Loaded(Arc::new(Image::from_bytes(format, bytes)))
                }
                None => ImageLoad::Failed,
            };
            let _ = this.update(cx, |this, cx| {
                this.images.insert(image, load);
                cx.notify();
            });
        })
        .detach();
        ImageLoad::Loading
    }
}

/// The labelled model catalog of `state`'s config; empty without one.
fn model_catalog(state: &ConnectionState) -> Arc<[ModelChoice]> {
    state
        .config
        .as_ref()
        .map(models::model_choices)
        .unwrap_or_default()
        .into()
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

/// The body of `url`, fetched on the tokio runtime (reqwest needs it).
async fn fetch_on(runtime: &tokio::runtime::Handle, url: String) -> Option<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
        let _ = tx.send(fetch(&url).await);
    });
    rx.await.ok().flatten()
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

/// The image format of `bytes`, from their signature.
fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    Some(match bytes {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [0xFF, 0xD8, 0xFF, ..] => ImageFormat::Jpeg,
        [b'G', b'I', b'F', b'8', ..] => ImageFormat::Gif,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        [b'B', b'M', ..] => ImageFormat::Bmp,
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => ImageFormat::Tiff,
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
            let head = head.trim_start_matches('\u{feff}').trim_start();
            if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
                ImageFormat::Svg
            } else {
                return None;
            }
        }
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

    #[test]
    fn markdown_images_resolve_to_urls_or_host_files() {
        let host = |path: &str| ImageRef::HostFile {
            thread_id: "T".into(),
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
        assert_eq!(sniff_format(&bytes), Some(ImageFormat::Png));
        let svg = data_url("data:image/svg+xml,%3Csvg%20xmlns='x'/%3E").unwrap();
        assert_eq!(sniff_format(&svg), Some(ImageFormat::Svg));
    }
}
