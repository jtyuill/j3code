//! Uploaded chat attachments (images and files) and the WS methods that
//! manage them: `attachments.createUploadUrl`, `assets.createUrl` and
//! `attachments.delete` (ios-surface M10, M11, M15).
//!
//! Layout: `dir/<id>.json` holds the declared metadata from
//! `createUploadUrl`; `dir/<id>` holds the bytes once uploaded (HTTP H4).
//! Images tools return are stored the same way ([`AttachmentStore::store_image`]).
//! Ids are random `[a-z0-9_]` strings and double as the upload and asset
//! tokens; there is no auth and no signing. Host image files
//! (`workspace-file` assets) are served through in-memory grants: a random
//! id that maps to the file's path until it expires.

use std::{
    collections::{HashMap, hash_map::RandomState},
    fmt::Write as _,
    hash::{BuildHasher, Hasher},
    io::{self, ErrorKind},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

use crate::rpc::Response;

/// `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES` (`chatAttachment.ts`).
pub const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/gif", "image/jpeg", "image/png", "image/webp"];
/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES`.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// `PROVIDER_SEND_TURN_MAX_FILE_BYTES`.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// `ATTACHMENT_UPLOAD_URL_TTL_MS` (`assets.ts`).
const UPLOAD_URL_TTL_MS: u64 = 10 * 60_000;
/// The reference server's asset URL lifetime (`ASSET_TOKEN_TTL_MS`).
const ASSET_URL_TTL_MS: u64 = 60 * 60_000;
/// `ChatAttachmentId` max length.
const MAX_ID_CHARS: usize = 128;
/// Last path segment the client treats as "no favicon"
/// (`PROJECT_FAVICON_FALLBACK_MARKER`).
const FAVICON_MISSING_URL: &str = "/api/assets/project-favicon/project-favicon-missing";

const UPLOAD_ROUTE_PREFIX: &str = "/api/attachments/upload";
const ASSET_ROUTE_PREFIX: &str = "/api/assets";

/// Attachment bytes and metadata on disk.
pub struct AttachmentStore {
    dir: PathBuf,
    /// Serializes publishing uploaded bytes, so each id is written once.
    publish: Mutex<()>,
    next_part: AtomicU64,
    /// Host image files granted by `assets.createUrl`, by grant id.
    grants: std::sync::Mutex<HashMap<String, Grant>>,
}

/// A host image file readable through `/api/assets/<id>/…` until `expires_at`.
struct Grant {
    path: PathBuf,
    mime_type: String,
    name: String,
    expires_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AttachmentKind {
    Image,
    File,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(rename = "type")]
    kind: AttachmentKind,
    name: String,
    mime_type: String,
    size_bytes: u64,
    created_at: u64,
}

/// An uploaded attachment, for serving it over HTTP.
pub(crate) struct StoredAttachment {
    pub path: PathBuf,
    pub size: u64,
    pub kind: AttachmentKind,
    pub mime_type: String,
    /// File name declared at upload.
    pub name: String,
}

/// Result of publishing upload bytes.
pub(crate) enum UploadOutcome {
    Stored,
    /// The id already holds different bytes.
    Conflict,
}

impl AttachmentStore {
    /// Opens (creating if needed) the store in `dir` and removes partial
    /// uploads left by an earlier process.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') && name.ends_with(".part") {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(Self {
            dir,
            publish: Mutex::new(()),
            next_part: AtomicU64::new(0),
            grants: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Handles `attachments.createUploadUrl`, `assets.createUrl`,
    /// `attachments.delete`; None for other tags.
    pub async fn handle(&self, tag: &str, payload: Value) -> Option<Response> {
        Some(match tag {
            "attachments.createUploadUrl" => self.create_upload_url(&payload).await,
            "assets.createUrl" => self.create_asset_url(&payload).await,
            "attachments.delete" => self.delete(&payload).await,
            _ => return None,
        })
    }

    /// Uploaded bytes + mime type, for sending images to omp.
    pub async fn read(&self, attachment_id: &str) -> io::Result<(Vec<u8>, String)> {
        let Some(meta) = self.meta(attachment_id).await? else {
            return Err(not_found(attachment_id));
        };
        let bytes = fs::read(self.dir.join(attachment_id)).await?;
        Ok((bytes, meta.mime_type))
    }

    /// Stores an image a tool returned as attachment `attachment_id`.
    pub(crate) async fn store_image(
        &self,
        attachment_id: &str,
        name: &str,
        mime_type: &str,
        bytes: &[u8],
    ) -> io::Result<()> {
        let meta = serde_json::to_vec(&Meta {
            kind: AttachmentKind::Image,
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size_bytes: bytes.len() as u64,
            created_at: now_ms(),
        })
        .map_err(io::Error::other)?;
        fs::write(self.meta_path(attachment_id), meta).await?;
        match self.store_upload(attachment_id, bytes).await? {
            UploadOutcome::Stored => Ok(()),
            UploadOutcome::Conflict => Err(io::Error::new(
                ErrorKind::AlreadyExists,
                format!("attachment {attachment_id} already holds other bytes"),
            )),
        }
    }

    /// The `assets.createUrl` result for attachment `attachment_id`, or None
    /// if it is unknown or its bytes have not arrived. `file_name` defaults
    /// to the name declared at upload.
    pub(crate) async fn attachment_url(
        &self,
        attachment_id: &str,
        file_name: Option<String>,
    ) -> io::Result<Option<Value>> {
        Ok(self.stored(attachment_id).await?.map(|stored| {
            json!({
                "relativeUrl": asset_url(attachment_id, &file_name.unwrap_or(stored.name)),
                "expiresAt": now_ms() + ASSET_URL_TTL_MS,
            })
        }))
    }

    /// Grants reading the host image file `path` for the asset URL lifetime;
    /// the `assets.createUrl` result.
    pub(crate) fn grant_file(&self, path: PathBuf, mime_type: &str) -> Value {
        let now = now_ms();
        let expires_at = now + ASSET_URL_TTL_MS;
        let name = path.file_name().map_or_else(
            || "image".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let id = new_id();
        let url = asset_url(&id, &name);
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        grants.retain(|_, grant| grant.expires_at > now);
        grants.insert(
            id,
            Grant {
                path,
                mime_type: mime_type.to_owned(),
                name,
                expires_at,
            },
        );
        json!({"relativeUrl": url, "expiresAt": expires_at})
    }

    /// The live grant `id`: its path, MIME type and file name.
    fn grant(&self, id: &str) -> Option<(PathBuf, String, String)> {
        let grants = self
            .grants
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        grants
            .get(id)
            .filter(|grant| grant.expires_at > now_ms())
            .map(|grant| {
                (
                    grant.path.clone(),
                    grant.mime_type.clone(),
                    grant.name.clone(),
                )
            })
    }

    /// Declared size of an upload-able id, or None if the id is unknown.
    pub(crate) async fn declared_size(&self, attachment_id: &str) -> io::Result<Option<u64>> {
        Ok(self.meta(attachment_id).await?.map(|meta| meta.size_bytes))
    }

    /// Publishes upload bytes (already checked against the declared size).
    /// Repeating an upload with identical bytes succeeds.
    pub(crate) async fn store_upload(
        &self,
        attachment_id: &str,
        bytes: &[u8],
    ) -> io::Result<UploadOutcome> {
        let final_path = self.dir.join(attachment_id);
        let _publishing = self.publish.lock().await;
        match fs::read(&final_path).await {
            Ok(existing) if existing == bytes => return Ok(UploadOutcome::Stored),
            Ok(_) => return Ok(UploadOutcome::Conflict),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let part = self.dir.join(format!(
            ".{attachment_id}.{}.part",
            self.next_part.fetch_add(1, Ordering::Relaxed)
        ));
        let written = async {
            fs::write(&part, bytes).await?;
            fs::rename(&part, &final_path).await
        }
        .await;
        if written.is_err() {
            let _ = fs::remove_file(&part).await;
        }
        written.map(|()| UploadOutcome::Stored)
    }

    /// The uploaded attachment (or granted host file) `attachment_id`, or
    /// None if it is unknown or its bytes have not arrived.
    pub(crate) async fn stored(&self, attachment_id: &str) -> io::Result<Option<StoredAttachment>> {
        if let Some((path, mime_type, name)) = self.grant(attachment_id) {
            return match fs::metadata(&path).await {
                Ok(file) if file.is_file() => Ok(Some(StoredAttachment {
                    path,
                    size: file.len(),
                    kind: AttachmentKind::Image,
                    mime_type,
                    name,
                })),
                Ok(_) => Ok(None),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            };
        }
        let Some(meta) = self.meta(attachment_id).await? else {
            return Ok(None);
        };
        let path = self.dir.join(attachment_id);
        match fs::metadata(&path).await {
            Ok(file) => Ok(Some(StoredAttachment {
                path,
                size: file.len(),
                kind: meta.kind,
                mime_type: meta.mime_type,
                name: meta.name,
            })),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn meta(&self, attachment_id: &str) -> io::Result<Option<Meta>> {
        if !is_valid_id(attachment_id) {
            return Ok(None);
        }
        match fs::read(self.meta_path(attachment_id)).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn meta_path(&self, attachment_id: &str) -> PathBuf {
        self.dir.join(format!("{attachment_id}.json"))
    }

    async fn create_upload_url(&self, payload: &Value) -> Response {
        let input = match UploadInput::decode(payload) {
            Ok(input) => input,
            Err(issue) => return Response::die(issue),
        };
        match self.create(input).await {
            Ok(attachment_id) => Response::ok(json!({
                "relativeUrl": format!("{UPLOAD_ROUTE_PREFIX}/{attachment_id}"),
                "attachmentId": attachment_id,
                "expiresAt": now_ms() + UPLOAD_URL_TTL_MS,
            })),
            Err(error) => Response::fail(json!({
                "_tag": "AttachmentUploadSigningKeyError",
                "cause": {"name": "Error", "message": error.to_string()},
            })),
        }
    }

    async fn create(&self, input: UploadInput) -> io::Result<String> {
        let meta = serde_json::to_vec(&Meta {
            kind: input.kind,
            name: input.name,
            mime_type: input.mime_type,
            size_bytes: input.size_bytes,
            created_at: now_ms(),
        })
        .map_err(io::Error::other)?;
        loop {
            let attachment_id = new_id();
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.meta_path(&attachment_id))
                .await;
            let mut file = match file {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            file.write_all(&meta).await?;
            file.flush().await?;
            return Ok(attachment_id);
        }
    }

    async fn create_asset_url(&self, payload: &Value) -> Response {
        let fields = match object(payload, "")
            .and_then(|input| required(input, "resource", ""))
            .and_then(|resource| object(resource, "[\"resource\"]"))
        {
            Ok(fields) => fields,
            Err(issue) => return Response::die(issue),
        };
        let not_found = |tag: &str| Response::fail(json!({"_tag": tag, "resource": fields}));
        match fields.get("_tag").and_then(Value::as_str) {
            Some("attachment") => {
                let input = match AttachmentResource::decode(fields) {
                    Ok(input) => input,
                    Err(issue) => return Response::die(issue),
                };
                match self
                    .attachment_url(&input.attachment_id, input.file_name)
                    .await
                {
                    Ok(Some(result)) => Response::ok(result),
                    Ok(None) => not_found("AssetAttachmentNotFoundError"),
                    Err(error) => Response::die(error.to_string()),
                }
            }
            Some("project-favicon") => match trimmed(fields, "cwd", 1024, "[\"resource\"]") {
                Ok(_) => Response::ok(json!({
                    "relativeUrl": FAVICON_MISSING_URL,
                    "expiresAt": now_ms() + ASSET_URL_TTL_MS,
                })),
                Err(issue) => Response::die(issue),
            },
            // `workspace-file` and `tool-output-image` need the thread; the
            // app answers them before this store is asked.
            Some("workspace-file" | "media-file" | "draft-workspace-file") => {
                not_found("AssetWorkspaceContextNotFoundError")
            }
            Some("tool-output-image") => not_found("AssetWorkspaceAssetNotFoundError"),
            Some("native-app-icon") => not_found("AssetPreviewTypeValidationError"),
            Some("github-media") => {
                Response::fail(json!({"_tag": "AssetGitHubMediaUrlValidationError"}))
            }
            _ => Response::die(issue(
                format!(
                    "Expected \"workspace-file\" | \"media-file\" | \"draft-workspace-file\" | \
                     \"attachment\" | \"tool-output-image\" | \"project-favicon\" | \
                     \"native-app-icon\" | \"github-media\", got {}",
                    shown(fields.get("_tag"))
                ),
                "[\"resource\"][\"_tag\"]",
            )),
        }
    }

    async fn delete(&self, payload: &Value) -> Response {
        let attachment_id =
            match object(payload, "").and_then(|input| trimmed(input, "attachmentId", 256, "")) {
                Ok(id) => id,
                Err(issue) => return Response::die(issue),
            };
        // An unknown id also counts as success (M15).
        if is_valid_id(&attachment_id) {
            let _publishing = self.publish.lock().await;
            for path in [
                self.dir.join(&attachment_id),
                self.meta_path(&attachment_id),
            ] {
                match fs::remove_file(path).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Response::die(error.to_string()),
                }
            }
        }
        Response::ok(Value::Null)
    }
}

/// Decoded `AttachmentCreateUploadUrlInput`.
struct UploadInput {
    kind: AttachmentKind,
    name: String,
    mime_type: String,
    size_bytes: u64,
}

impl UploadInput {
    fn decode(payload: &Value) -> Result<Self, String> {
        let input = object(payload, "")?;
        let kind = match input.get("type") {
            None => AttachmentKind::Image,
            Some(Value::String(kind)) if kind == "image" => AttachmentKind::Image,
            Some(Value::String(kind)) if kind == "file" => AttachmentKind::File,
            Some(other) => {
                return Err(issue(
                    format!("Expected \"image\" | \"file\", got {other}"),
                    "[\"type\"]",
                ));
            }
        };
        let name = trimmed(input, "name", 255, "")?;
        let (mime_type, max_bytes) = match kind {
            AttachmentKind::Image => {
                let mime_type = required(input, "mimeType", "")?;
                match mime_type.as_str() {
                    Some(mime) if SUPPORTED_IMAGE_MIME_TYPES.contains(&mime) => {
                        (mime.to_owned(), MAX_IMAGE_BYTES)
                    }
                    _ => {
                        return Err(issue(
                            format!(
                                "Expected \"image/gif\" | \"image/jpeg\" | \"image/png\" | \
                                 \"image/webp\", got {mime_type}"
                            ),
                            "[\"mimeType\"]",
                        ));
                    }
                }
            }
            AttachmentKind::File => (trimmed(input, "mimeType", 100, "")?, MAX_FILE_BYTES),
        };
        let size_bytes = integer_between(input, "sizeBytes", 1, max_bytes)?;
        Ok(Self {
            kind,
            name,
            mime_type,
            size_bytes,
        })
    }
}

/// Decoded `AssetResource` `attachment` variant.
struct AttachmentResource {
    attachment_id: String,
    file_name: Option<String>,
}

impl AttachmentResource {
    fn decode(fields: &Map<String, Value>) -> Result<Self, String> {
        const AT: &str = "[\"resource\"]";
        let attachment_id = trimmed(fields, "attachmentId", 256, AT)?;
        let file_name = optional_trimmed(fields, "fileName", 255, AT)?;
        optional_trimmed(fields, "mimeType", 100, AT)?;
        optional_literal(fields, "disposition", &["inline", "attachment"], AT)?;
        optional_literal(fields, "renderIntent", &["mcp-app"], AT)?;
        Ok(Self {
            attachment_id,
            file_name,
        })
    }
}

// Payload checks. Failures are reported like effect's schema issue formatter
// (`<message>\n  at <path>`) and returned as a Die on the request.

fn issue(message: String, path: &str) -> String {
    if path.is_empty() {
        message
    } else {
        format!("{message}\n  at {path}")
    }
}

fn shown(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_owned(), Value::to_string)
}

fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| issue(format!("Expected object, got {value}"), at))
}

fn required<'a>(fields: &'a Map<String, Value>, key: &str, at: &str) -> Result<&'a Value, String> {
    fields
        .get(key)
        .ok_or_else(|| issue("Missing key".to_owned(), &format!("{at}[\"{key}\"]")))
}

/// `TrimmedNonEmptyString` with a max length (UTF-16 units, like JS).
fn trimmed(fields: &Map<String, Value>, key: &str, max: usize, at: &str) -> Result<String, String> {
    let value = required(fields, key, at)?;
    check_trimmed(value, max, &format!("{at}[\"{key}\"]"))
}

fn optional_trimmed(
    fields: &Map<String, Value>,
    key: &str,
    max: usize,
    at: &str,
) -> Result<Option<String>, String> {
    fields
        .get(key)
        .map(|value| check_trimmed(value, max, &format!("{at}[\"{key}\"]")))
        .transpose()
}

fn check_trimmed(value: &Value, max: usize, path: &str) -> Result<String, String> {
    let Some(text) = value.as_str() else {
        return Err(issue(format!("Expected string, got {value}"), path));
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(issue(
            format!("Expected a non-blank string, got {value}"),
            path,
        ));
    }
    if text.encode_utf16().count() > max {
        return Err(issue(
            format!("Expected a value with a length of at most {max}, got {value}"),
            path,
        ));
    }
    Ok(text.to_owned())
}

fn optional_literal(
    fields: &Map<String, Value>,
    key: &str,
    literals: &[&str],
    at: &str,
) -> Result<(), String> {
    match fields.get(key) {
        None => Ok(()),
        Some(Value::String(text)) if literals.contains(&text.as_str()) => Ok(()),
        Some(other) => {
            let expected = literals
                .iter()
                .map(|literal| format!("\"{literal}\""))
                .collect::<Vec<_>>()
                .join(" | ");
            Err(issue(
                format!("Expected {expected}, got {other}"),
                &format!("{at}[\"{key}\"]"),
            ))
        }
    }
}

/// A JS integer (`5.0` counts) within `min..=max`.
fn integer_between(
    fields: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    let path = format!("[\"{key}\"]");
    let value = required(fields, key, "")?;
    let Some(number) = value.as_f64() else {
        return Err(issue(format!("Expected number, got {value}"), &path));
    };
    if number.fract() != 0.0 || !number.is_finite() {
        return Err(issue(format!("Expected an integer, got {value}"), &path));
    }
    if number < min as f64 || number > max as f64 {
        return Err(issue(
            format!("Expected a value between {min} and {max}, got {value}"),
            &path,
        ));
    }
    Ok(number as u64)
}

/// Ids this store issues: `ChatAttachmentId`-compatible, lowercase only so
/// they map to one file on case-insensitive filesystems.
pub(crate) fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && id
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// `att_` + 128 random bits in hex. `RandomState` keys come from the OS
/// randomness source; ids are unguessable enough to act as bearer-less
/// tokens, which is all the no-auth design needs.
pub(crate) fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut id = String::with_capacity(4 + 32);
    id.push_str("att_");
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        let _ = write!(id, "{:016x}", hasher.finish());
    }
    id
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn not_found(attachment_id: &str) -> io::Error {
    io::Error::new(
        ErrorKind::NotFound,
        format!("attachment {attachment_id} was not uploaded"),
    )
}

/// `/api/assets/<id>/<file name, percent-encoded as one path segment>`.
fn asset_url(attachment_id: &str, file_name: &str) -> String {
    let mut url = format!("{ASSET_ROUTE_PREFIX}/{attachment_id}/");
    for byte in file_name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            url.push(byte as char);
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}
