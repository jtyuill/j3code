//! Images in the draft (spec §7.3): pasted (a clipboard image or image files) or dropped,
//! shown at once as chips and uploaded to the composer's environment as they arrive.

use std::path::{Path, PathBuf};

use j3_client::t3::chat_attachment::ChatAttachment;
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

const UNSUPPORTED: &str = "Paste a PNG, JPEG, WebP or GIF image.";
const TOO_BIG: &str = "Images must be 10 MB or smaller.";

pub enum Upload {
    Uploading,
    Done(ChatAttachment),
    Failed(String),
}

/// An image in the draft.
pub struct PendingImage {
    pub id: u64,
    pub name: String,
    image: ImageLoad,
    pub upload: Upload,
}

/// The attachments to send, or why the draft can't be sent yet.
pub(super) fn attachments(images: &[PendingImage]) -> Result<Vec<ChatAttachment>, &'static str> {
    images
        .iter()
        .map(|image| match &image.upload {
            Upload::Done(attachment) => Ok(attachment.clone()),
            Upload::Uploading => Err("Wait for images to finish uploading."),
            Upload::Failed(_) => Err("Remove the images that failed to upload."),
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

impl J3 {
    /// Adds `bytes` to `key`'s draft as `name` (or `pasted-{n}.{ext}`) and starts its
    /// upload; refuses formats other than PNG/JPEG/WebP/GIF and images over 10 MB.
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
        let composer = self.composer(key);
        let id = composer.next_image;
        composer.next_image += 1;
        let extension = mime.trim_start_matches("image/").replace("jpeg", "jpg");
        let name = name.unwrap_or_else(|| format!("pasted-{}.{extension}", id + 1));
        composer.error = None;
        composer.images.push(PendingImage {
            id,
            name: name.clone(),
            image,
            upload: Upload::Uploading,
        });
        let Some(connection) = self.envs.get(key.env()).map(|e| e.connection().clone()) else {
            self.image_uploaded(key, id, Err("The environment is gone.".to_owned()));
            return;
        };
        let key = key.clone();
        self.spawn(async move {
            let result = connection
                .upload_image(&name, mime, bytes)
                .await
                .map_err(|e| e.to_string());
            move |s: &mut J3, tx: &mut Tx| {
                s.image_uploaded(&key, id, result);
                tx.instant();
            }
        });
    }

    fn image_uploaded(
        &mut self,
        key: &ComposerKey,
        id: u64,
        result: Result<ChatAttachment, String>,
    ) {
        let Some(composer) = self.composers.get_mut(key) else {
            return;
        };
        if let Some(image) = composer.images.iter_mut().find(|image| image.id == id) {
            image.upload = match result {
                Ok(attachment) => Upload::Done(attachment),
                Err(error) => Upload::Failed(error),
            };
        }
    }

    /// A paste into `key`'s field: a clipboard image or image files become images; false
    /// lets the editor paste the text.
    pub(super) fn composer_paste(
        &mut self,
        key: &ComposerKey,
        content: &ClipboardContent,
        _: &mut Tx,
    ) -> bool {
        let files: Vec<PathBuf> = content
            .files()
            .iter()
            .filter(|p| is_image_path(p))
            .cloned()
            .collect();
        let image = content.image().map(|image| image.png().to_vec());
        if image.is_none() && files.is_empty() {
            return false;
        }
        if let Some(png) = image {
            self.add_composer_image(key, png, None);
        }
        self.read_image_files(key, files);
        true
    }

    /// Files dropped on `key`'s composer.
    pub(super) fn composer_drop(&mut self, key: &ComposerKey, paths: &[PathBuf]) {
        let (images, other): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.iter().cloned().partition(|p| is_image_path(p));
        if !other.is_empty() {
            self.composer(key).error = Some(UNSUPPORTED.to_owned());
        }
        self.read_image_files(key, images);
    }

    /// Reads image files off the UI thread, then adds them under their file names.
    fn read_image_files(&mut self, key: &ComposerKey, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let key = key.clone();
        self.spawn_blocking(move || {
            let read: Vec<(Vec<u8>, Option<String>)> = paths
                .iter()
                .filter_map(|path| {
                    let bytes = std::fs::read(path).ok()?;
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    Some((bytes, name))
                })
                .collect();
            move |s: &mut J3, tx: &mut Tx| {
                // The draft may have gone while the files were read.
                if !s.composers.contains_key(&key) {
                    return;
                }
                for (bytes, name) in read {
                    s.add_composer_image(&key, bytes, name);
                }
                tx.instant();
            }
        });
    }

    fn remove_composer_image(&mut self, key: &ComposerKey, id: u64) {
        self.composer(key).images.retain(|image| image.id != id);
    }
}

/// The draft's images: thumbnail, name, upload state and a remove button each.
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey) -> Option<El> {
    let composer = s.composers.get(key)?;
    if composer.images.is_empty() {
        return None;
    }
    let k = ui.k;
    let danger = Status::of(k).danger;
    let radius = k.radius.sm;
    let chips = composer.images.iter().map(|image| {
        let id = image.id;
        let failed = match &image.upload {
            Upload::Failed(error) => Some(error.as_str()),
            _ => None,
        };
        let thumb = el()
            .key("thumb")
            .size(40.0, 40.0)
            .radius(radius)
            .clip_rounded(radius)
            .fill(k.c.layer_02);
        let thumb = match &image.image {
            ImageLoad::Loaded(img) if img.is_ready() => {
                thumb.pixels_fit(img.pixels(), SurfaceFit::Fill)
            }
            _ => thumb,
        };
        let mut names = col().key("name").max_w(ui.m.rems(9.0)).min_w(0.0).child(
            text(&image.name)
                .font(k.ty.text_xsmall)
                .color(k.c.text_base)
                .truncate(),
        );
        if let Some(error) = failed {
            names = names.child(text(error).font(k.ty.text_xsmall).color(danger).truncate());
        }
        let remove_key = key.clone();
        let remove = row()
            .key(format!("remove-image-{id}"))
            .role("button")
            .label("Remove image")
            .help("Remove image")
            .size(20.0, 20.0)
            .center()
            .radius(radius)
            .hover(k.c.hover)
            .child(icon(&icons::X, 12.0).color(k.c.icon_muted))
            .on_press(move |s: &mut J3| s.remove_composer_image(&remove_key, id));
        let mut chip = row()
            .key(format!("composer-image-{id}"))
            .label(&image.name)
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
        if matches!(image.upload, Upload::Uploading) {
            chip = chip.child(Spinner::new().size(12.0).build(k));
        }
        if let Some(error) = failed {
            chip = chip.help(error);
        }
        chip.child(remove)
    });
    Some(
        row()
            .key("composer-images")
            .gap(8.0)
            .px(12.0)
            .pt(8.0)
            .flex_wrap()
            .children(chips),
    )
}
