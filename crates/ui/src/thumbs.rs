//! Downscaled decodes for transcript images. A full-page screenshot can be
//! 1280×16000 (80 MB as a bitmap); a row shows it at 360×225. These decode
//! once on a background thread, scale to about twice the row's size, and
//! live in gpui's asset cache keyed by path and box, so scrolling back to a
//! row never decodes again.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Asset, ImageCacheError, ImageSource, RenderImage};

/// One downscaled decode: `path` fitted into `w`×`h` device pixels, either
/// inside the box (`cover: false`) or filling it (`cover: true`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbSource {
    pub path: PathBuf,
    pub w: u32,
    pub h: u32,
    pub cover: bool,
}

/// The asset loader for [`ThumbSource`].
pub enum Thumb {}

impl Asset for Thumb {
    type Source = ThumbSource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        _cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { decode(&source) }
    }
}

/// The box a decoded `iw`×`ih` image is scaled to: never up, and kept to
/// `w`×`h` (inside it, or covering it for `cover`).
pub fn fit(iw: u32, ih: u32, w: u32, h: u32, cover: bool) -> (u32, u32) {
    if iw == 0 || ih == 0 {
        return (iw, ih);
    }
    let (sx, sy) = (w as f64 / iw as f64, h as f64 / ih as f64);
    let scale = if cover { sx.max(sy) } else { sx.min(sy) }.min(1.0);
    (
        ((iw as f64 * scale).round() as u32).max(1),
        ((ih as f64 * scale).round() as u32).max(1),
    )
}

fn decode(source: &ThumbSource) -> Result<Arc<RenderImage>, ImageCacheError> {
    let bytes = std::fs::read(&source.path)?;
    let image = image::load_from_memory(&bytes)?;
    let (tw, th) = fit(
        image.width(),
        image.height(),
        source.w,
        source.h,
        source.cover,
    );
    let image = if (tw, th) == (image.width(), image.height()) {
        image
    } else {
        image.resize_exact(tw, th, image::imageops::FilterType::Triangle)
    };
    let mut rgba = image.into_rgba8();
    // gpui wants BGRA.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

/// An image source for `path` shown in a `w`×`h` (logical px) box: decoded
/// at twice that size at most, off the main thread, cached.
pub fn source(path: impl Into<PathBuf>, w: f32, h: f32, cover: bool) -> ImageSource {
    let source = ThumbSource {
        path: path.into(),
        w: (w * 2.0).ceil().max(1.0) as u32,
        h: (h * 2.0).ceil().max(1.0) as u32,
        cover,
    };
    ImageSource::Custom(Arc::new(move |window, cx| {
        window.use_asset::<Thumb>(&source, cx)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_upscales_and_keeps_aspect() {
        // A full-page shot into a 720×450 box: contained by height.
        assert_eq!(fit(1280, 5600, 720, 450, false), (103, 450));
        // A viewport shot: contained by width.
        assert_eq!(fit(1280, 800, 720, 450, false), (720, 450));
        // Smaller than the box: untouched.
        assert_eq!(fit(300, 200, 720, 450, false), (300, 200));
        // Cover fills the box on both axes.
        let (w, h) = fit(1280, 5600, 200, 200, true);
        assert!(w >= 200 && h >= 200 && w == 200);
    }

    #[test]
    fn decode_scales_down_a_large_png() {
        let dir = std::env::temp_dir().join(format!("keel-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.png");
        image::RgbaImage::from_pixel(1600, 1000, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let image = decode(&ThumbSource {
            path: path,
            w: 720,
            h: 450,
            cover: false,
        })
        .unwrap();
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (720, 450));
        // BGRA: blue first.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[30, 20, 10, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
