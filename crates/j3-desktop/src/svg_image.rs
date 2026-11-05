//! SVGs rasterized at exactly the device-pixel size they are drawn at.
//!
//! GPUI's `img()` rasterizes an SVG once, at twice its intrinsic size, and
//! lets the GPU scale that bitmap to the element. Without mipmaps a large
//! bitmap shrunk several times over drops detail and aliases, worst on 1x
//! displays: a 64 px logo drawn at 16 px samples one texel in eight. This
//! renders the SVG for the element's size times the window's scale factor
//! instead, so every texel lands on one device pixel.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gpui_kit::{
    App, DevicePixels, Global, ImageCacheError, ImageSource, Pixels, RenderImage, Size, SvgSize,
    Window, size,
};

/// Rasters kept before the oldest are dropped from the GPU atlas.
const CAPACITY: usize = 512;

type Key = (u64, i32, i32);

#[derive(Default)]
struct Rasters {
    images: HashMap<Key, Arc<RenderImage>>,
    order: VecDeque<Key>,
}

impl Global for Rasters {}

/// An image source drawing `svg` at `size` logical pixels, re-rasterized
/// whenever the window's scale factor gives a different device size.
///
/// `key` identifies the SVG's content: equal keys must mean equal bytes.
pub fn exact<B>(key: u64, svg: B, size: Size<Pixels>) -> ImageSource
where
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    ImageSource::Custom(Arc::new(move |window, cx| {
        Some(raster(key, svg.as_ref(), size, window, cx))
    }))
}

fn raster(
    key: u64,
    svg: &[u8],
    logical: Size<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let scale = window.scale_factor();
    let device = |pixels: Pixels| (f32::from(pixels) * scale).ceil().max(1.) as i32;
    let (width, height) = (device(logical.width), device(logical.height));
    let key = (key, width, height);
    if let Some(image) = cx
        .try_global::<Rasters>()
        .and_then(|rasters| rasters.images.get(&key))
    {
        return Ok(image.clone());
    }

    let renderer = cx.svg_renderer();
    let image = renderer
        .parse_svg(svg)
        .and_then(|parsed| {
            renderer.render_parsed(
                &parsed,
                SvgSize::ExactSize(size(DevicePixels(width), DevicePixels(height))),
            )
        })
        .map_err(|error| ImageCacheError::Asset(error.to_string().into()))?;

    let rasters = cx.default_global::<Rasters>();
    rasters.images.insert(key, image.clone());
    rasters.order.push_back(key);
    let mut evicted = Vec::new();
    while rasters.order.len() > CAPACITY {
        if let Some(old) = rasters.order.pop_front()
            && let Some(old) = rasters.images.remove(&old)
        {
            evicted.push(old);
        }
    }
    for old in evicted {
        cx.drop_image(old, Some(window));
    }
    Ok(image)
}
