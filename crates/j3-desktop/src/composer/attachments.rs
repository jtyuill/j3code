//! Attachments in the draft (spec §7.3): images and files, pasted (a clipboard image or
//! copied files) or dropped, shown at once as chips and uploaded to the composer's
//! environment as they arrive. omp sees an image inline; any other file is read into the
//! prompt by the server naming it in an `@` mention, like a path the user typed.

use std::path::{Path, PathBuf};

use j3_client::entities::Attachment;
use rune::{ClipboardContent, El, SurfaceFit, Tx, col, el, icon, row, text};
use rune_kit::Spinner;

use super::ComposerKey;
use crate::{
    app::{J3, Ui},
    icons,
    images::{self, Format, ImageLoad},
    theme::Status,
};

/// Largest image the composer uploads (the server's image limit).
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Largest other file the composer uploads (the server's file limit).
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

const UNSUPPORTED: &str = "Paste a PNG, JPEG, WebP or GIF image.";
const TOO_BIG: &str = "Images must be 10 MB or smaller.";
const FILE_TOO_BIG: &str = "Files must be 50 MB or smaller.";
const UNREADABLE: &str = "That file can't be read.";
/// The MIME type of an uploaded file that is not an image the composer takes.
const FILE_MIME: &str = "application/octet-stream";

pub enum Upload {
    Uploading,
    Done(Attachment),
    Failed(String),
}

/// An image or a file in the draft.
pub struct PendingAttachment {
    pub id: u64,
    pub name: String,
    /// An image's picture; None for any other file.
    image: Option<ImageLoad>,
    pub upload: Upload,
}

/// The ids of the attachments to send, or why the draft can't be sent yet.
pub(super) fn attachments(pending: &[PendingAttachment]) -> Result<Vec<String>, &'static str> {
    pending
        .iter()
        .map(|attachment| match &attachment.upload {
            Upload::Done(attachment) => Ok(attachment.id.clone()),
            Upload::Uploading => Err("Wait for attachments to finish uploading."),
            Upload::Failed(_) => Err("Remove the attachments that failed to upload."),
        })
        .collect()
}

/// The upload format of `bytes`: PNG, JPEG, WebP or GIF.
fn accepted(bytes: &[u8]) -> Option<Format> {
    images::sniff(bytes).filter(|format| {
        matches!(
            format,
            Format::Png | Format::Jpeg | Format::Webp | Format::Gif
        )
    })
}

/// Whether `path` names an image the composer takes, by its extension.
fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif"
        )
    })
}

/// A pasted or dropped file, read off the UI thread.
enum Read {
    Image(Vec<u8>, String),
    File(Vec<u8>, String),
    Refused(&'static str),
}

/// Reads `path` for the draft: an image by its extension, else a file of at most
/// [`MAX_FILE_BYTES`]. A folder is not attached.
fn read(path: &Path) -> Read {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
    let image = is_image_path(path);
    match std::fs::metadata(path) {
        Ok(file) if file.is_dir() => return Read::Refused("Folders can't be attached."),
        Ok(file) if !image && file.len() > MAX_FILE_BYTES => return Read::Refused(FILE_TOO_BIG),
        Ok(file) if image && file.len() > MAX_IMAGE_BYTES as u64 => {
            return Read::Refused(TOO_BIG);
        }
        Ok(_) => {}
        Err(_) => return Read::Refused(UNREADABLE),
    }
    match std::fs::read(path) {
        Ok(bytes) if image => Read::Image(bytes, name),
        Ok(bytes) => Read::File(bytes, name),
        Err(_) => Read::Refused(UNREADABLE),
    }
}

impl J3 {
    /// Adds `bytes` to `key`'s draft as the image `name` (or `pasted-{n}.{ext}`) and starts
    /// its upload; refuses formats other than PNG/JPEG/WebP/GIF and images over 10 MB.
    pub fn add_composer_image(&mut self, key: &ComposerKey, bytes: Vec<u8>, name: Option<String>) {
        let Some(format) = accepted(&bytes) else {
            self.composer(key).error = Some(UNSUPPORTED.to_owned());
            return;
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            self.composer(key).error = Some(TOO_BIG.to_owned());
            return;
        }
        let mime = format.mime();
        let image = self.images.decode(bytes.clone());
        let extension = mime.trim_start_matches("image/").replace("jpeg", "jpg");
        let next = self.composer(key).next_attachment + 1;
        let name = name.unwrap_or_else(|| format!("pasted-{next}.{extension}"));
        self.upload_attachment(key, name, mime, bytes, Some(image));
    }

    /// Adds `bytes` to `key`'s draft as the file `name` and starts its upload; refuses
    /// files over 50 MB.
    pub fn add_composer_file(&mut self, key: &ComposerKey, bytes: Vec<u8>, name: String) {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            self.composer(key).error = Some(FILE_TOO_BIG.to_owned());
            return;
        }
        self.upload_attachment(key, name, FILE_MIME, bytes, None);
    }

    /// Shows the attachment in `key`'s draft and uploads it to the draft's environment.
    fn upload_attachment(
        &mut self,
        key: &ComposerKey,
        name: String,
        mime: &'static str,
        bytes: Vec<u8>,
        image: Option<ImageLoad>,
    ) {
        let composer = self.composer(key);
        let id = composer.next_attachment;
        composer.next_attachment += 1;
        composer.error = None;
        composer.attachments.push(PendingAttachment {
            id,
            name: name.clone(),
            image,
            upload: Upload::Uploading,
        });
        let Some(connection) = self.envs.get(key.env()).map(|e| e.connection().clone()) else {
            self.attachment_uploaded(key, id, Err("The environment is gone.".to_owned()));
            return;
        };
        let key = key.clone();
        self.spawn(async move {
            let result = connection
                .upload(&name, mime, bytes)
                .await
                .map_err(|e| e.to_string());
            move |s: &mut J3, tx: &mut Tx| {
                s.attachment_uploaded(&key, id, result);
                tx.instant();
            }
        });
    }

    fn attachment_uploaded(
        &mut self,
        key: &ComposerKey,
        id: u64,
        result: Result<Attachment, String>,
    ) {
        let Some(composer) = self.composers.get_mut(key) else {
            return;
        };
        if let Some(attachment) = composer.attachments.iter_mut().find(|a| a.id == id) {
            attachment.upload = match result {
                Ok(attachment) => Upload::Done(attachment),
                Err(error) => Upload::Failed(error),
            };
        }
    }

    /// A paste into `key`'s field: a clipboard image or copied files become attachments;
    /// false lets the editor paste the text.
    pub(super) fn composer_paste(
        &mut self,
        key: &ComposerKey,
        content: &ClipboardContent,
        _: &mut Tx,
    ) -> bool {
        let files = content.files().to_vec();
        let image = content.image().map(|image| image.png().to_vec());
        if image.is_none() && files.is_empty() {
            return false;
        }
        if let Some(png) = image {
            self.add_composer_image(key, png, None);
        }
        self.read_files(key, files);
        true
    }

    /// Files dropped on `key`'s composer.
    pub(super) fn composer_drop(&mut self, key: &ComposerKey, paths: &[PathBuf]) {
        self.read_files(key, paths.to_vec());
    }

    /// Reads files off the UI thread, then adds them under their file names.
    fn read_files(&mut self, key: &ComposerKey, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let key = key.clone();
        self.spawn_blocking(move || {
            let read: Vec<Read> = paths.iter().map(|path| read(path)).collect();
            move |s: &mut J3, tx: &mut Tx| {
                // The draft may have gone while the files were read.
                if !s.composers.contains_key(&key) {
                    return;
                }
                let mut refused = None;
                for file in read {
                    match file {
                        Read::Image(bytes, name) => s.add_composer_image(&key, bytes, Some(name)),
                        Read::File(bytes, name) => s.add_composer_file(&key, bytes, name),
                        Read::Refused(why) => refused = Some(why),
                    }
                }
                // Adding an attachment clears the note; a refusal among them stays.
                if let Some(why) = refused {
                    s.composer(&key).error = Some(why.to_owned());
                }
                tx.instant();
            }
        });
    }

    fn remove_composer_attachment(&mut self, key: &ComposerKey, id: u64) {
        self.composer(key).attachments.retain(|a| a.id != id);
    }
}

/// The draft's attachments: thumbnail (a file glyph for a file), name, upload state and a
/// remove button each.
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey) -> Option<El> {
    let composer = s.composers.get(key)?;
    if composer.attachments.is_empty() {
        return None;
    }
    let k = ui.k;
    let danger = Status::of(k).danger;
    let radius = k.radius.sm;
    let chips = composer.attachments.iter().map(|attachment| {
        let id = attachment.id;
        let failed = match &attachment.upload {
            Upload::Failed(error) => Some(error.as_str()),
            _ => None,
        };
        let thumb = el()
            .key("thumb")
            .size(40.0, 40.0)
            .radius(radius)
            .clip_rounded(radius)
            .fill(k.c.layer_02);
        let thumb = match &attachment.image {
            Some(ImageLoad::Loaded(img)) if img.is_ready() => {
                thumb.pixels_fit(img.pixels(), SurfaceFit::Fill)
            }
            Some(_) => thumb,
            None => thumb
                .center()
                .child(icon(&icons::FILE, 18.0).color(k.c.icon_muted)),
        };
        let mut names = col().key("name").max_w(ui.m.rems(9.0)).min_w(0.0).child(
            text(&attachment.name)
                .font(k.ty.text_xsmall)
                .color(k.c.text_base)
                .truncate(),
        );
        if let Some(error) = failed {
            names = names.child(text(error).font(k.ty.text_xsmall).color(danger).truncate());
        }
        let remove_key = key.clone();
        let remove = row()
            .key(format!("remove-attachment-{id}"))
            .role("button")
            .label("Remove attachment")
            .help("Remove attachment")
            .size(20.0, 20.0)
            .center()
            .radius(radius)
            .hover(k.c.hover)
            .child(icon(&icons::X, 12.0).color(k.c.icon_muted))
            .on_press(move |s: &mut J3| s.remove_composer_attachment(&remove_key, id));
        let mut chip = row()
            .key(format!("composer-attachment-{id}"))
            .label(&attachment.name)
            .gap(4.0)
            .padding(4.0)
            .center_y()
            .radius(radius)
            .border(
                1.0,
                if failed.is_some() {
                    danger
                } else {
                    k.c.border_base
                },
            )
            .child(thumb)
            .child(names);
        if matches!(attachment.upload, Upload::Uploading) {
            chip = chip.child(Spinner::new().size(12.0).build(k));
        }
        if let Some(error) = failed {
            chip = chip.help(error);
        }
        chip.child(remove)
    });
    Some(
        row()
            .key("composer-attachments")
            .gap(8.0)
            .px(12.0)
            .pt(8.0)
            .flex_wrap()
            .children(chips),
    )
}
