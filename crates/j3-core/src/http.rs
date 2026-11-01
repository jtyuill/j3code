//! HTTP surface t3code clients use (ios-surface §1.3): the environment
//! descriptor, the RPC WebSocket, the auth-session constant, attachment
//! upload and asset download, and 501 stubs for the HTTP snapshot endpoints.

use std::{io::SeekFrom, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, RawQuery, State, WebSocketUpgrade, ws::rejection::WebSocketUpgradeRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{
    attachments::{AttachmentKind, AttachmentStore, UploadOutcome},
    rpc::{self, RpcHandler},
};

/// `ORCHESTRATION_PROTOCOL_VERSION` (`environment.ts`).
const ORCHESTRATION_PROTOCOL_VERSION: u32 = 2;
/// The same version as the `orchestrationProtocol` query value.
const ORCHESTRATION_PROTOCOL: &str = "2";
/// Bytes per body chunk when streaming an asset.
const ASSET_CHUNK_BYTES: usize = 64 * 1024;

pub struct HttpState {
    /// ExecutionEnvironmentDescriptor JSON (ios-surface H1); static for the
    /// process.
    pub descriptor: Value,
    pub rpc: Arc<dyn RpcHandler>,
    pub attachments: Arc<AttachmentStore>,
}

struct Shared {
    /// The descriptor, encoded once: clients poll it as a liveness check.
    descriptor: Bytes,
    auth_session: Bytes,
    rpc: Arc<dyn RpcHandler>,
    attachments: Arc<AttachmentStore>,
}

/// `ServerAuthDescriptor` for j3: no auth at all.
pub fn auth_descriptor() -> Value {
    json!({
        "policy": "unsafe-no-auth",
        "bootstrapMethods": [],
        "sessionMethods": [],
        "sessionCookieName": "j3_session",
    })
}

/// The constant `AuthSessionState` (ios-surface H3). The permissions are what
/// the client checks locally before guarded writes, uploads and browsing.
pub fn auth_session_state() -> Value {
    json!({
        "authenticated": true,
        "auth": auth_descriptor(),
        "scopes": ["orchestration:read", "orchestration:operate"],
        "permissions": [
            "orchestration:read",
            "orchestration:operate",
            "filesystem:read",
            "filesystem:write",
            "settings:write",
        ],
    })
}

pub fn router(state: HttpState) -> Router {
    let shared = Arc::new(Shared {
        descriptor: Bytes::from(state.descriptor.to_string()),
        auth_session: Bytes::from(auth_session_state().to_string()),
        rpc: state.rpc,
        attachments: state.attachments,
    });
    Router::new()
        .route("/.well-known/t3/environment", get(environment))
        .route("/ws", get(websocket))
        .route("/api/auth/session", get(auth_session))
        .route("/api/attachments/upload/{attachment_id}", post(upload))
        .route("/api/assets/{attachment_id}/{*file_name}", get(asset))
        .route("/api/orchestration/shell", get(not_implemented))
        .route(
            "/api/orchestration/threads/{thread_id}/bounded",
            get(not_implemented),
        )
        .route(
            "/api/orchestration/threads/{thread_id}/history",
            get(not_implemented),
        )
        .with_state(shared)
}

fn json_bytes(body: Bytes) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

async fn environment(State(shared): State<Arc<Shared>>) -> Response {
    json_bytes(shared.descriptor.clone())
}

async fn auth_session(State(shared): State<Arc<Shared>>) -> Response {
    json_bytes(shared.auth_session.clone())
}

/// H6–H8: the client falls back to the socket on any failure. A plain-text
/// body keeps the bounded-snapshot client from reading it as "thread missing".
async fn not_implemented() -> Response {
    text(StatusCode::NOT_IMPLEMENTED, "Not Implemented")
}

/// H2. The client must ask for orchestration protocol 2 (first
/// `orchestrationProtocol` query value, like `URLSearchParams.get`).
async fn websocket(
    State(shared): State<Arc<Shared>>,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let requested = query.as_deref().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == "orchestrationProtocol").then_some(value)
        })
    });
    if requested != Some(ORCHESTRATION_PROTOCOL) {
        return (
            StatusCode::UPGRADE_REQUIRED,
            axum::Json(json!({
                "code": "orchestration_protocol_incompatible",
                "message": format!(
                    "Update this client to one that supports orchestration protocol \
                     {ORCHESTRATION_PROTOCOL_VERSION}."
                ),
                "orchestrationProtocolVersion": ORCHESTRATION_PROTOCOL_VERSION,
            })),
        )
            .into_response();
    }
    match upgrade {
        Ok(upgrade) => {
            let handler = shared.rpc.clone();
            upgrade.on_upgrade(move |socket| rpc::serve_socket(socket, handler))
        }
        Err(rejection) => rejection.into_response(),
    }
}

/// H4: raw bytes for an id issued by `attachments.createUploadUrl`.
async fn upload(
    State(shared): State<Arc<Shared>>,
    Path(attachment_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let size = match shared.attachments.declared_size(&attachment_id).await {
        Ok(Some(size)) => size,
        Ok(None) => return text(StatusCode::NOT_FOUND, "Not Found"),
        Err(error) => {
            tracing::error!(%error, attachment_id, "reading attachment metadata failed");
            return text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist upload.",
            );
        }
    };
    let length_mismatch = headers.get(header::CONTENT_LENGTH).is_some_and(|length| {
        length
            .to_str()
            .ok()
            .and_then(|length| length.parse::<u64>().ok())
            != Some(size)
    });
    if length_mismatch {
        return text(
            StatusCode::BAD_REQUEST,
            "Content-Length must match the upload size.",
        );
    }
    // Sizes are capped at 50 MiB by `createUploadUrl`.
    let Ok(bytes) = axum::body::to_bytes(body, size as usize).await else {
        return text(
            StatusCode::BAD_REQUEST,
            "Body is larger than the upload size.",
        );
    };
    if bytes.len() as u64 != size {
        return text(StatusCode::BAD_REQUEST, "Body must match the upload size.");
    }
    match shared
        .attachments
        .store_upload(&attachment_id, &bytes)
        .await
    {
        Ok(UploadOutcome::Stored) => StatusCode::NO_CONTENT.into_response(),
        Ok(UploadOutcome::Conflict) => text(StatusCode::CONFLICT, "Attachment already uploaded."),
        Err(error) => {
            tracing::error!(%error, attachment_id, "persisting attachment upload failed");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist upload.",
            )
        }
    }
}

/// H5: an uploaded attachment, with single-range `Range`/`If-Range`
/// support. Images are served inline; other files as downloads named by the
/// URL's file name segment.
async fn asset(
    State(shared): State<Arc<Shared>>,
    method: Method,
    Path((attachment_id, file_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let stored = match shared.attachments.stored(&attachment_id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return text(StatusCode::NOT_FOUND, "Not Found"),
        Err(error) => {
            tracing::error!(%error, attachment_id, "reading attachment failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        }
    };
    // Bytes behind an id never change, so the id is a strong validator.
    let etag = format!("\"{attachment_id}\"");
    let mut response_headers = HeaderMap::new();
    let mut set = |name: header::HeaderName, value: &str| {
        if let Ok(value) = HeaderValue::from_str(value) {
            response_headers.insert(name, value);
        }
    };
    set(header::CACHE_CONTROL, "private, max-age=3600");
    set(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(header::ACCEPT_RANGES, "bytes");
    set(header::ETAG, &etag);
    match stored.kind {
        AttachmentKind::Image => set(header::CONTENT_TYPE, &stored.mime_type),
        AttachmentKind::File => {
            let content_type = if is_safe_download_mime_type(&stored.mime_type) {
                stored.mime_type.as_str()
            } else {
                "application/octet-stream"
            };
            set(header::CONTENT_TYPE, content_type);
            set(
                header::CONTENT_DISPOSITION,
                &download_content_disposition(&file_name),
            );
            set(
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            );
        }
    }

    let size = stored.size;
    let mut status = StatusCode::OK;
    let mut offset = 0;
    let mut length = size;
    // Range is defined for GET only. If-Range needs an exact strong match;
    // anything else (dates, weak tags) gets the full body, which is safe.
    let range = headers
        .get(header::RANGE)
        .filter(|_| method == Method::GET)
        .and_then(|range| range.to_str().ok())
        .filter(|_| {
            headers
                .get(header::IF_RANGE)
                .is_none_or(|if_range| if_range.as_bytes() == etag.as_bytes())
        })
        .and_then(|range| parse_byte_range(range, size));
    match range {
        Some(ByteRange::Unsatisfiable) => {
            set(header::CONTENT_RANGE, &format!("bytes */{size}"));
            return (StatusCode::RANGE_NOT_SATISFIABLE, response_headers).into_response();
        }
        Some(ByteRange::Satisfiable { start, end }) => {
            status = StatusCode::PARTIAL_CONTENT;
            offset = start;
            length = end - start + 1;
            set(
                header::CONTENT_RANGE,
                &format!("bytes {start}-{end}/{size}"),
            );
        }
        None => {}
    }
    set(header::CONTENT_LENGTH, &length.to_string());

    let mut file = match tokio::fs::File::open(&stored.path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, attachment_id, "opening attachment failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        }
    };
    if offset > 0
        && let Err(error) = file.seek(SeekFrom::Start(offset)).await
    {
        tracing::error!(%error, attachment_id, "seeking attachment failed");
        return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
    }
    let body = stream::unfold(file.take(length), |mut reader| async move {
        let mut chunk = Vec::with_capacity(ASSET_CHUNK_BYTES);
        match reader.read_buf(&mut chunk).await {
            Ok(0) => None,
            Ok(_) => Some((Ok(Bytes::from(chunk)), reader)),
            Err(error) => Some((Err(error), reader)),
        }
    });
    (status, response_headers, Body::from_stream(body)).into_response()
}

#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    /// Inclusive byte offsets.
    Satisfiable {
        start: u64,
        end: u64,
    },
    Unsatisfiable,
}

/// One `bytes=first-last` / `bytes=first-` / `bytes=-suffix` range, as the
/// reference server's `assetByteRange` reads it. None means "ignore the
/// header" (malformed or multi-range): the full body is sent.
fn parse_byte_range(header: &str, size: u64) -> Option<ByteRange> {
    let spec = header.trim();
    let prefix = spec.get(..6)?;
    if !prefix.eq_ignore_ascii_case("bytes=") {
        return None;
    }
    let (first, last) = spec[6..].split_once('-')?;
    let first = parse_digits(first)?;
    let last = parse_digits(last)?;
    if first.is_none() && last.is_none() {
        return None;
    }
    if let (Some(first), Some(last)) = (first, last)
        && last < first
    {
        return None;
    }
    if size == 0 || first.is_some_and(|first| first >= size) || (first.is_none() && last == Some(0))
    {
        return Some(ByteRange::Unsatisfiable);
    }
    let (start, end) = match (first, last) {
        (Some(first), Some(last)) => (first, last.min(size - 1)),
        (Some(first), None) => (first, size - 1),
        (None, Some(suffix)) => (size.saturating_sub(suffix), size - 1),
        (None, None) => return None,
    };
    Some(ByteRange::Satisfiable { start, end })
}

/// `\d*`: Some(None) when empty, saturating on overflow (an offset that
/// large is past any file anyway).
fn parse_digits(digits: &str) -> Option<Option<u64>> {
    if digits.is_empty() {
        return Some(None);
    }
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(Some(digits.bytes().fold(0u64, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(u64::from(digit - b'0'))
    })))
}

/// `isSafeDownloadMimeType` (reference `http.ts`): a plain `type/subtype`
/// that is not HTML or XML.
fn is_safe_download_mime_type(mime_type: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'_' | b'!' | b'#' | b'$' | b'&' | b'^' | b'.' | b'+' | b'-'
                    )
            })
    };
    let Some((kind, subtype)) = mime_type.split_once('/') else {
        return false;
    };
    if !token(kind) || !token(subtype) {
        return false;
    }
    let lower = mime_type.to_ascii_lowercase();
    !(lower == "text/html"
        || lower.ends_with("/xml")
        || lower.contains("/xml-")
        || lower.ends_with("+xml"))
}

/// `downloadContentDisposition` (reference `http.ts`).
fn download_content_disposition(file_name: &str) -> String {
    let sanitized: String = file_name
        .chars()
        .map(|c| {
            if c.is_control() || c == '"' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let ascii: String = sanitized
        .chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '_' })
        .collect();
    if ascii == sanitized {
        return format!("attachment; filename=\"{ascii}\"");
    }
    let mut extended = String::with_capacity(sanitized.len() * 3);
    for byte in sanitized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~') {
            extended.push(byte as char);
        } else {
            extended.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{extended}")
}
