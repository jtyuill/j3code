//! The HTTP surface (`docs/protocol/wire.md` §1, §2, §8): discovery, the v2
//! WebSocket, attachment upload and download, and granted project images.

use std::{io::SeekFrom, sync::Arc};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        Path, Query, RawQuery, State, WebSocketUpgrade, ws::rejection::WebSocketUpgradeRejection,
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use j3_protocol::v2::{
    PROTOCOL,
    entities::Attachment,
    envelope::{Error, ErrorCode},
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{
    app::App,
    attachments::{
        AttachmentKind, AttachmentStore, MAX_FILE_BYTES, MAX_IMAGE_BYTES,
        SUPPORTED_IMAGE_MIME_TYPES,
    },
    v2::{driver, router::Router as V2Router},
};

/// Bytes per body chunk when streaming an attachment.
const CHUNK_BYTES: usize = 64 * 1024;

struct Shared {
    app: Arc<App>,
    router: Arc<V2Router>,
    attachments: Arc<AttachmentStore>,
}

/// Every route of a j3 server.
pub fn router(app: Arc<App>) -> Router {
    let shared = Arc::new(Shared {
        router: Arc::new(V2Router::new(Arc::clone(&app))),
        attachments: app.attachments(),
        app,
    });
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/ws", get(websocket))
        .route("/api/v1/attachments", post(upload))
        .route("/api/v1/attachments/{attachment_id}", get(download))
        .route("/api/v1/assets/{token}", get(download))
        .with_state(shared)
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

fn error(status: StatusCode, error: Error) -> Response {
    (status, Json(error)).into_response()
}

async fn info(State(shared): State<Arc<Shared>>) -> Response {
    Json(shared.app.info()).into_response()
}

/// The upgrade must ask for this server's protocol (wire.md §1).
async fn websocket(
    State(shared): State<Arc<Shared>>,
    RawQuery(query): RawQuery,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let protocol = PROTOCOL.to_string();
    let requested = query.as_deref().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == "protocol").then_some(value)
        })
    });
    if requested != Some(protocol.as_str()) {
        return (
            StatusCode::UPGRADE_REQUIRED,
            Json(json!({
                "code": "protocol_mismatch",
                "message": format!("This server speaks j3 protocol {PROTOCOL}."),
                "protocol": PROTOCOL,
            })),
        )
            .into_response();
    }
    match upgrade {
        Ok(upgrade) => {
            let handler: Arc<dyn driver::Handler> = shared.router.clone();
            upgrade.on_upgrade(move |socket| driver::serve_socket(socket, handler))
        }
        Err(rejection) => rejection.into_response(),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

/// `POST /api/v1/attachments?name=…` (wire.md §8).
async fn upload(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let bad = |message: &str| error(StatusCode::BAD_REQUEST, Error::bad_request(message));
    let Some(name) = query
        .name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
    else {
        return bad("The `name` query parameter is required.");
    };
    let mime_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    let Some(size) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return bad("Content-Length is required.");
    };
    let image = SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type.as_str());
    let limit = if image {
        MAX_IMAGE_BYTES
    } else {
        MAX_FILE_BYTES
    };
    if size > limit {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            Error::new(
                ErrorCode::TooLarge,
                format!("The file is larger than {} MiB.", limit >> 20),
            )
            .with_data(json!({"limit": limit})),
        );
    }
    if size == 0 {
        return bad("The file is empty.");
    }
    let attachment_id = match shared
        .app
        .inner()
        .create_attachment(image, &name, &mime_type, size)
        .await
    {
        Ok(id) => id,
        Err(failure) => return error(StatusCode::INTERNAL_SERVER_ERROR, failure),
    };
    let Ok(bytes) = axum::body::to_bytes(body, size as usize).await else {
        return bad("The body is larger than Content-Length.");
    };
    if bytes.len() as u64 != size {
        return bad("The body must match Content-Length.");
    }
    if let Err(message) = shared.attachments.store_new(&attachment_id, &bytes).await {
        tracing::error!(%message, attachment_id, "storing upload failed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            Error::internal("The upload could not be stored."),
        );
    }
    (
        StatusCode::CREATED,
        Json(Attachment {
            id: attachment_id,
            name,
            mime_type,
            size,
        }),
    )
        .into_response()
}

/// `GET /api/v1/attachments/<id>` and `GET /api/v1/assets/<token>`.
async fn download(
    State(shared): State<Arc<Shared>>,
    method: Method,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve_attachment(&shared.attachments, &method, &id, &headers).await
}

/// Serves attachment (or granted project image) `attachment_id` with
/// single-range `Range` support. Images are served inline (sandboxed, so an
/// SVG opened in a browser runs no script), other files as downloads with a
/// sandboxing CSP.
async fn serve_attachment(
    attachments: &AttachmentStore,
    method: &Method,
    attachment_id: &str,
    headers: &HeaderMap,
) -> Response {
    let stored = match attachments.stored(attachment_id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return text(StatusCode::NOT_FOUND, "Not Found"),
        Err(error) => {
            tracing::error!(%error, attachment_id, "reading attachment failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        }
    };
    let file_name = &stored.name;
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
        AttachmentKind::Image => {
            set(header::CONTENT_TYPE, &stored.mime_type);
            set(header::CONTENT_SECURITY_POLICY, "sandbox");
        }
        AttachmentKind::File => {
            let content_type = if is_safe_download_mime_type(&stored.mime_type) {
                stored.mime_type.as_str()
            } else {
                "application/octet-stream"
            };
            set(header::CONTENT_TYPE, content_type);
            set(
                header::CONTENT_DISPOSITION,
                &download_content_disposition(file_name),
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
        .filter(|_| *method == Method::GET)
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
        let mut chunk = Vec::with_capacity(CHUNK_BYTES);
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

/// One `bytes=first-last` / `bytes=first-` / `bytes=-suffix` range. None
/// means "ignore the header" (malformed or multi-range): the full body is
/// sent.
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

/// A plain `type/subtype` that is not HTML or XML, safe to serve as is.
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

/// `Content-Disposition` of a download named `file_name`, with an RFC 5987
/// name when it is not plain ASCII.
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
