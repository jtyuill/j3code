//! Attachments (`docs/protocol/wire.md` §8): uploaded images and files, and
//! images omp's tools return.
//!
//! Layout: `dir/<id>.json` holds the metadata declared at upload; `dir/<id>`
//! holds the bytes; `dir/<id>.d/<name>` links to the bytes under the upload's
//! file name once omp is to read the file ([`AttachmentStore::named_path`]).
//! Ids are `[a-z0-9_-]` strings and the only capability
//! needed to read the bytes; there is no auth. Host image files of a
//! project (`projects.imageUrl`) are served through in-memory grants: a
//! random id that maps to the file's path until it expires.

use std::{
    collections::{HashMap, hash_map::RandomState},
    fmt::Write as _,
    hash::{BuildHasher, Hasher},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use j3_protocol::v2::entities::Attachment;
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

/// Image types omp accepts.
pub const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/gif", "image/jpeg", "image/png", "image/webp"];
/// Largest image upload.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// Largest file upload.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// How long a project image grant lasts.
const GRANT_TTL_MS: u64 = 60 * 60_000;
/// Longest attachment id.
const MAX_ID_CHARS: usize = 128;

/// Attachment bytes and metadata on disk.
pub struct AttachmentStore {
    dir: PathBuf,
    /// Serializes publishing uploaded bytes, so each id is written once.
    publish: Mutex<()>,
    next_part: AtomicU64,
    /// Project image files granted by `projects.imageUrl`, by grant id.
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

    /// Uploaded bytes + mime type, for sending images to omp.
    pub async fn read(&self, attachment_id: &str) -> io::Result<(Vec<u8>, String)> {
        let Some(meta) = self.meta(attachment_id).await? else {
            return Err(not_found(attachment_id));
        };
        let bytes = fs::read(self.dir.join(attachment_id)).await?;
        Ok((bytes, meta.mime_type))
    }

    /// A path to upload `attachment_id` ending in its file name, for an omp
    /// `@"path"` mention: omp picks how to read a file (PDF, document,
    /// video…) by its extension, and the model sees the name. A hard link to
    /// the bytes, or a copy where the filesystem has none.
    pub(crate) async fn named_path(&self, attachment_id: &str) -> io::Result<PathBuf> {
        let Some(meta) = self.meta(attachment_id).await? else {
            return Err(not_found(attachment_id));
        };
        let dir = std::path::absolute(self.dir.join(format!("{attachment_id}.d")))?;
        let path = dir.join(mention_name(&meta.name));
        if fs::try_exists(&path).await? {
            return Ok(path);
        }
        fs::create_dir_all(&dir).await?;
        let bytes = self.dir.join(attachment_id);
        match fs::hard_link(&bytes, &path).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Err(error),
            Err(_) => {
                fs::copy(&bytes, &path).await?;
            }
        }
        Ok(path)
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

    /// Grants reading the host image file `path` for an hour: the grant id
    /// (readable like an attachment id) and its expiry in epoch
    /// milliseconds.
    pub(crate) fn grant_path(&self, path: PathBuf, mime_type: &str) -> (String, u64) {
        let now = now_ms();
        let expires_at = now + GRANT_TTL_MS;
        let name = path.file_name().map_or_else(
            || "image".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let id = new_id();
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        grants.retain(|_, grant| grant.expires_at > now);
        grants.insert(
            id.clone(),
            Grant {
                path,
                mime_type: mime_type.to_owned(),
                name,
                expires_at,
            },
        );
        (id, expires_at)
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

    /// Publishes the bytes of an id created moments ago for a one-shot
    /// upload (v2 `POST /api/v1/attachments`).
    pub(crate) async fn store_new(&self, attachment_id: &str, bytes: &[u8]) -> Result<(), String> {
        match self.store_upload(attachment_id, bytes).await {
            Ok(UploadOutcome::Stored) => Ok(()),
            Ok(UploadOutcome::Conflict) => {
                Err(format!("{attachment_id} already holds other bytes"))
            }
            Err(error) => Err(error.to_string()),
        }
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

    /// Declares a new upload: its metadata is written now, its bytes by
    /// [`Self::store_new`]. Returns the new id.
    pub(crate) async fn create(
        &self,
        kind: AttachmentKind,
        name: &str,
        mime_type: &str,
        size: u64,
    ) -> io::Result<String> {
        let meta = serde_json::to_vec(&Meta {
            kind,
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size_bytes: size,
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

    /// `attachments.delete`; deleting an unknown id succeeds.
    pub async fn delete(&self, attachment_id: &str) -> io::Result<()> {
        if !is_valid_id(attachment_id) {
            return Ok(());
        }
        let _publishing = self.publish.lock().await;
        for path in [self.dir.join(attachment_id), self.meta_path(attachment_id)] {
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match fs::remove_dir_all(self.dir.join(format!("{attachment_id}.d"))).await {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The wire entity of an uploaded attachment, None if it is unknown or
    /// its bytes have not arrived.
    pub async fn attachment(&self, attachment_id: &str) -> io::Result<Option<Attachment>> {
        if self.grant(attachment_id).is_some() {
            return Ok(None);
        }
        Ok(self.stored(attachment_id).await?.map(|stored| Attachment {
            id: attachment_id.to_owned(),
            name: stored.name,
            mime_type: stored.mime_type,
            size: stored.size,
        }))
    }
}

/// An upload's file name as the last component of an `@"path"` mention:
/// path separators, control characters and `"` (which would end the
/// mention) become `_`.
fn mention_name(name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | '"' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    match name.as_str() {
        "" | "." | ".." => "file".to_owned(),
        _ => name,
    }
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
/// randomness source; ids are unguessable enough to be the only capability
/// needed to read an attachment (wire.md §8).
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

/// The SHA-256 (hex) an omp image block names when omp keeps the bytes in
/// its blob store (`data: "blob:sha256:<hex>"`) instead of inline base64.
pub fn blob_hash(data: &str) -> Option<&str> {
    data.strip_prefix("blob:sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The id of an image omp produced, derived from its bytes so a replayed
/// session keeps one file per image: `att_` + the first 128 bits of the
/// bytes' SHA-256. `data` is omp's image data (base64 or a blob
/// reference); None when it is neither.
pub fn content_id(data: &str) -> Option<String> {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    if let Some(hex) = blob_hash(data) {
        return Some(format!("att_{}", hex[..32].to_ascii_lowercase()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .ok()?;
    let digest = Sha256::digest(&bytes);
    let mut id = String::with_capacity(4 + 32);
    id.push_str("att_");
    for byte in &digest[..16] {
        let _ = write!(id, "{byte:02x}");
    }
    Some(id)
}

/// The bytes of omp image data: inline base64, or a blob in the `blobs`
/// directory of the omp agent directory holding `session_file`.
pub fn image_bytes(data: &str, session_file: Option<&Path>) -> io::Result<Vec<u8>> {
    use base64::Engine as _;
    let Some(hex) = blob_hash(data) else {
        return base64::engine::general_purpose::STANDARD
            .decode(data.as_bytes())
            .map_err(io::Error::other);
    };
    let blobs = session_file
        .into_iter()
        .flat_map(Path::ancestors)
        .map(|dir| dir.join("blobs"))
        .find(|dir| dir.is_dir())
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "no omp blob directory"))?;
    std::fs::read(blobs.join(hex.to_ascii_lowercase()))
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
