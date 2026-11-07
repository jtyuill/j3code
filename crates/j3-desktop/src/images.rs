//! Decoded raster images the chat shows (attachments, tool output images, markdown images),
//! as `rune::Image` handles from one `rune::ImageCache`, kept in a bounded LRU: at most
//! [`MAX_ENTRIES`] images and [`MAX_BYTES`] decoded bytes. Rune frees an image's pixels when
//! the last handle drops, so evicting an entry releases it once no element draws it.
//!
//! Views ask through `J3::image(env, &ImageRef)` (`environment.rs`), which reads this cache
//! and records a fetch for a miss; the app's `poll` starts recorded fetches. Bytes are sniffed
//! by signature (PNG, JPEG, GIF, WebP, BMP, TIFF, SVG) before decoding.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use rune::{El, Image, ImageCache, ImageId, Waker, el};

use crate::environment::ImageRef;

/// Images kept at most.
pub const MAX_ENTRIES: usize = 512;
/// Decoded RGBA bytes kept at most (images still decoding count as nothing).
pub const MAX_BYTES: usize = 256 * 1024 * 1024;
/// Device pixels per point an SVG rasterizes at: sharp on a 2× screen, mipped down on 1×.
pub const SVG_SCALE: f32 = 2.0;
/// An SVG's size when it names none, as a browser sizes a replaced element.
const SVG_DEFAULT: (f32, f32) = (300.0, 150.0);

/// `image` drawn `width` × `height` points, narrowing with its shape kept when the column
/// is narrower; corners rounded `radius`. The picture's height follows the width it gets,
/// not the column's (an aspect ratio on a full-width box clamped by `max_w` would keep the
/// column's height and leave a gap under it).
pub fn fitted(image: &Image, width: f32, height: f32, radius: f32) -> El {
    el().w_full().max_w(width).child(
        el().w_full()
            .aspect_ratio(width / height)
            .radius(radius)
            .pixels(image.pixels()),
    )
}

/// An image being fetched for display.
#[derive(Clone, Debug)]
pub enum ImageLoad {
    /// Fetching or decoding: draw a placeholder (a spinner) until the app wakes.
    Loading,
    /// Draw with `El::pixels(image.pixels())`; `image.is_ready()` turns true once decoded.
    Loaded(Image),
    Failed,
}

/// An image's encoding, from its signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Tiff,
    Svg,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Svg => "image/svg+xml",
        }
    }
}

/// The image format of `bytes`, from their signature.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    Some(match bytes {
        [0x89, b'P', b'N', b'G', ..] => Format::Png,
        [0xFF, 0xD8, 0xFF, ..] => Format::Jpeg,
        [b'G', b'I', b'F', b'8', ..] => Format::Gif,
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
        ] => Format::Webp,
        [b'B', b'M', ..] => Format::Bmp,
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Format::Tiff,
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
            let head = head.trim_start_matches('\u{feff}').trim_start();
            if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
                Format::Svg
            } else {
                return None;
            }
        }
    })
}

/// One server's image.
pub type ImageKey = (String, ImageRef);

struct Entry {
    load: ImageLoad,
    used: Cell<u64>,
}

pub struct Images {
    cache: ImageCache,
    entries: HashMap<ImageKey, Entry>,
    /// Misses views asked for since the last `poll`.
    wanted: RefCell<Vec<ImageKey>>,
    /// Device pixels per point of images not drawn a pixel per point (SVGs), by image id.
    densities: RefCell<HashMap<ImageId, f32>>,
    clock: Cell<u64>,
    max_entries: usize,
    max_bytes: usize,
}

impl Images {
    /// `waker` wakes the app when a decode finishes (`Inbox::waker`).
    pub fn new(waker: Waker) -> Self {
        Self::with_limits(waker, MAX_ENTRIES, MAX_BYTES)
    }

    pub fn with_limits(waker: Waker, max_entries: usize, max_bytes: usize) -> Self {
        Self {
            cache: ImageCache::new(waker),
            entries: HashMap::new(),
            wanted: RefCell::new(Vec::new()),
            densities: RefCell::default(),
            clock: Cell::new(0),
            max_entries,
            max_bytes,
        }
    }

    /// The shared decoder, for images the app holds itself (composer thumbnails, math).
    pub fn cache(&self) -> &ImageCache {
        &self.cache
    }

    /// The cached image for `key`, marking it used; on a miss, records the fetch (for
    /// [`Self::take_wanted`]) and returns `None`.
    pub fn get(&self, key: &ImageKey) -> Option<ImageLoad> {
        let tick = self.clock.get() + 1;
        self.clock.set(tick);
        match self.entries.get(key) {
            Some(entry) => {
                entry.used.set(tick);
                Some(entry.load.clone())
            }
            None => {
                let mut wanted = self.wanted.borrow_mut();
                if !wanted.contains(key) {
                    wanted.push(key.clone());
                }
                None
            }
        }
    }

    /// The misses recorded since the last call.
    pub fn take_wanted(&self) -> Vec<ImageKey> {
        std::mem::take(&mut *self.wanted.borrow_mut())
    }

    /// Stores `load` for `key`, then evicts the least recently used entries over the bounds.
    pub fn set(&mut self, key: ImageKey, load: ImageLoad) {
        let tick = self.clock.get() + 1;
        self.clock.set(tick);
        self.entries.insert(
            key,
            Entry {
                load,
                used: Cell::new(tick),
            },
        );
        self.evict();
    }

    /// Decodes `bytes` (sniffed first) into an image handle; `Failed` for unknown formats.
    /// Raster formats draw a pixel per point, as a browser draws them at 1×; an SVG
    /// rasterizes at [`SVG_SCALE`] pixels per point of its own size, so it stays sharp on a
    /// HiDPI screen when drawn at that size ([`Self::density`]).
    pub fn decode(&self, bytes: Vec<u8>) -> ImageLoad {
        match sniff(&bytes) {
            Some(Format::Svg) => {
                let (width, height) = svg_size(&bytes);
                let side = width.max(height);
                let image = self.cache.svg(bytes, side, SVG_SCALE);
                // `ImageCache::svg` rounds the side up and caps it at 1024 pixels.
                let pixels = (side * SVG_SCALE).ceil().clamp(1.0, 1024.0);
                self.densities
                    .borrow_mut()
                    .insert(image.id(), pixels / side);
                ImageLoad::Loaded(image)
            }
            Some(_) => ImageLoad::Loaded(self.cache.load(bytes)),
            None => ImageLoad::Failed,
        }
    }

    /// Device pixels per point `image` draws at for its natural size: 1 for raster images.
    pub fn density(&self, image: &Image) -> f32 {
        self.densities
            .borrow()
            .get(&image.id())
            .copied()
            .unwrap_or(1.0)
    }

    /// Forgets every image of environment `env` (it was removed).
    pub fn forget_env(&mut self, env: &str) {
        self.entries.retain(|(key, _), _| key != env);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn bytes(&self) -> usize {
        self.entries.values().map(|e| decoded_bytes(&e.load)).sum()
    }

    fn evict(&mut self) {
        let mut bytes = self.bytes();
        while self.entries.len() > self.max_entries || bytes > self.max_bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used.get())
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                bytes -= decoded_bytes(&entry.load);
            }
        }
        self.prune_densities();
    }

    /// Drops the densities of images no entry holds any more.
    fn prune_densities(&mut self) {
        let held: std::collections::HashSet<ImageId> = self
            .entries
            .values()
            .filter_map(|entry| match &entry.load {
                ImageLoad::Loaded(image) => Some(image.id()),
                _ => None,
            })
            .collect();
        self.densities.get_mut().retain(|id, _| held.contains(id));
    }
}

/// An SVG's own size in points: its root's `width` and `height` (px, pt or plain numbers),
/// else its `viewBox`, else a browser's 300 × 150.
fn svg_size(bytes: &[u8]) -> (f32, f32) {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let Some(start) = text.find("<svg") else {
        return SVG_DEFAULT;
    };
    let tag = &text[start..];
    let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
    let attribute = |name: &str| {
        let mut rest = tag;
        while let Some(at) = rest.find(name) {
            let before = rest[..at].chars().last();
            let after = rest[at + name.len()..].trim_start();
            rest = &rest[at + name.len()..];
            if !before.is_some_and(char::is_whitespace) {
                continue;
            }
            let Some(value) = after.strip_prefix('=') else {
                continue;
            };
            let value = value.trim_start();
            let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let value = &value[1..];
            return Some(value[..value.find(quote)?].trim().to_owned());
        }
        None
    };
    let length = |value: Option<String>| {
        let value = value?;
        let (number, scale) = if let Some(n) = value.strip_suffix("pt") {
            (n, 4.0 / 3.0)
        } else {
            (value.strip_suffix("px").unwrap_or(&value), 1.0)
        };
        number
            .trim()
            .parse::<f32>()
            .ok()
            .map(|n| n * scale)
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let view_box = attribute("viewBox").and_then(|vb| {
        let numbers: Vec<f32> = vb
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        match numbers[..] {
            [_, _, w, h] if w > 0.0 && h > 0.0 => Some((w, h)),
            _ => None,
        }
    });
    match (
        length(attribute("width")),
        length(attribute("height")),
        view_box,
    ) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((vw, vh))) => (w, w * vh / vw),
        (None, Some(h), Some((vw, vh))) => (h * vw / vh, h),
        (_, _, Some(size)) => size,
        _ => SVG_DEFAULT,
    }
}

fn decoded_bytes(load: &ImageLoad) -> usize {
    match load {
        ImageLoad::Loaded(image) => image.size().map_or(0, |[w, h]| w as usize * h as usize * 4),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn key(n: usize) -> ImageKey {
        (
            "local".into(),
            ImageRef::Url(format!("https://x.dev/{n}.png")),
        )
    }

    #[test]
    fn signatures_name_the_format() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), Some(Format::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"GIF89a"), Some(Format::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(sniff(b"BM\0\0"), Some(Format::Bmp));
        assert_eq!(sniff(b"II*\0"), Some(Format::Tiff));
        assert_eq!(
            sniff(b"\xef\xbb\xbf <svg xmlns='x'/>".as_slice()),
            Some(Format::Svg)
        );
        assert_eq!(sniff("\u{feff}<svg/>".as_bytes()), Some(Format::Svg));
        assert_eq!(sniff(b"<?xml version='1.0'?><svg/>"), Some(Format::Svg));
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn misses_are_recorded_once_and_the_oldest_entry_goes_first() {
        let mut images = Images::with_limits(Arc::new(|| {}), 2, MAX_BYTES);
        assert!(images.get(&key(1)).is_none());
        assert!(images.get(&key(1)).is_none());
        assert_eq!(images.take_wanted(), [key(1)]);
        assert!(images.take_wanted().is_empty());
        images.set(key(1), ImageLoad::Failed);
        images.set(key(2), ImageLoad::Loading);
        // Using 1 makes 2 the least recently used.
        assert!(images.get(&key(1)).is_some());
        images.set(key(3), ImageLoad::Loading);
        assert_eq!(images.len(), 2);
        assert!(images.get(&key(2)).is_none());
        assert!(images.get(&key(1)).is_some());
        images.forget_env("local");
        assert_eq!(images.len(), 0);
    }

    #[test]
    fn an_svg_takes_its_own_size_and_rasterizes_at_twice_it() {
        let svg = |attrs: &str| format!("<svg xmlns='http://www.w3.org/2000/svg' {attrs}/>");
        assert_eq!(
            svg_size(svg("width='240' height='72'").as_bytes()),
            (240.0, 72.0)
        );
        assert_eq!(
            svg_size(svg(r#"width="30pt" height="15px""#).as_bytes()),
            (40.0, 15.0)
        );
        assert_eq!(
            svg_size(svg("viewBox='0 0 100 50'").as_bytes()),
            (100.0, 50.0)
        );
        assert_eq!(
            svg_size(svg("width='200' viewBox='0 0 100 50'").as_bytes()),
            (200.0, 100.0)
        );
        assert_eq!(svg_size(svg("stroke-width='3'").as_bytes()), SVG_DEFAULT);

        let images = Images::new(Arc::new(|| {}));
        let ImageLoad::Loaded(image) = images.decode(svg("width='240' height='72'").into_bytes())
        else {
            panic!("an SVG decodes");
        };
        assert_eq!(image.wait().unwrap(), [480, 144]);
        assert_eq!(images.density(&image), 2.0);
    }
}
