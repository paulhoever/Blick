//! Decoding with EXIF orientation and fast downscaling.

use std::path::Path;
use std::sync::OnceLock;

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::error::{CoreError, Result};
use crate::formats::{Kind, kind_of};
use crate::raw;

/// RGBA8-Pixelpuffer, dicht gepackt (stride = width * 4).
/// The format maps 1:1 to `gdk::MemoryFormat::R8g8b8a8`.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixels({}x{})", self.width, self.height)
    }
}

impl Pixels {
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    pub fn from_dynamic(img: DynamicImage) -> Self {
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        Self {
            width,
            height,
            data: rgba.into_raw(),
        }
    }

    pub fn into_dynamic(self) -> DynamicImage {
        let buf = image::RgbaImage::from_raw(self.width, self.height, self.data)
            .expect("Pixels always has width*height*4 bytes");
        DynamicImage::ImageRgba8(buf)
    }

    /// Shrinks so the image fits in `max_w` x `max_h`. Never enlarges.
    pub fn fit_within(self, max_w: u32, max_h: u32) -> Result<Self> {
        let (w, h) = fit_size(self.width, self.height, max_w, max_h);
        if (w, h) == (self.width, self.height) {
            return Ok(self);
        }
        let src = FirImage::from_vec_u8(self.width, self.height, self.data, PixelType::U8x4)
            .map_err(|e| CoreError::Resize(e.to_string()))?;
        let mut dst = FirImage::new(w, h, PixelType::U8x4);
        // Bilinear is enough for display and a lot faster than Lanczos.
        let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
        Resizer::new()
            .resize(&src, &mut dst, &opts)
            .map_err(|e| CoreError::Resize(e.to_string()))?;
        Ok(Self {
            width: w,
            height: h,
            data: dst.into_vec(),
        })
    }
}

/// Target size keeping the aspect ratio, at least 1x1.
pub fn fit_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let scale = f64::min(max_w as f64 / w as f64, max_h as f64 / h as f64);
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// Decoder for everything neither `image` nor `rawler` can handle. The app
/// registers glycin here; without a registration such files count as
/// unsupported.
pub type Fallback = dyn Fn(&Path) -> Result<DynamicImage> + Send + Sync;

static FALLBACK: OnceLock<Box<Fallback>> = OnceLock::new();

pub fn set_fallback(f: impl Fn(&Path) -> Result<DynamicImage> + Send + Sync + 'static) {
    let _ = FALLBACK.set(Box::new(f));
}

/// Grabs a still frame from a video. The app registers GStreamer; without
/// a registration videos have no thumbnail.
pub type FrameGrabber = dyn Fn(&Path, u32) -> Result<DynamicImage> + Send + Sync;

static FRAME_GRABBER: OnceLock<Box<FrameGrabber>> = OnceLock::new();

pub fn set_frame_grabber(f: impl Fn(&Path, u32) -> Result<DynamicImage> + Send + Sync + 'static) {
    let _ = FRAME_GRABBER.set(Box::new(f));
}

/// Still frame from the video, at most `max_edge` pixels on the longer edge.
pub fn video_frame(path: &Path, max_edge: u32) -> Result<DynamicImage> {
    let f = FRAME_GRABBER
        .get()
        .ok_or_else(|| CoreError::Unsupported(path.to_owned()))?;
    f(path, max_edge)
}

fn fallback(path: &Path) -> Result<DynamicImage> {
    let f = FALLBACK
        .get()
        .ok_or_else(|| CoreError::Unsupported(path.to_owned()))?;
    f(path)
}

/// Largest edge length up to which an embedded RAW preview is enough.
const RAW_PREVIEW_MIN: u32 = 1024;

/// Decodes fully and applies the EXIF orientation.
/// RAW is fully developed here; export and editing need that.
pub fn load_oriented(path: &Path) -> Result<DynamicImage> {
    match kind_of(path) {
        Some(Kind::Raw) => raw::develop(path),
        Some(Kind::Other) => fallback(path),
        Some(Kind::Video) => video_frame(path, 4096),
        // Whatever `image` can't handle (exotic TIFF, say) goes to glycin.
        _ => load_raster(path).or_else(|e| fallback(path).map_err(|_| e)),
    }
}

fn load_raster(path: &Path) -> Result<DynamicImage> {
    let reader = ImageReader::open(path)
        .map_err(|e| CoreError::io(path, e))?
        .with_guessed_format()
        .map_err(|e| CoreError::io(path, e))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| CoreError::image(path, e))?;
    // Missing or broken EXIF data is no reason not to show the picture.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| CoreError::image(path, e))?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Dimensions of the original after orientation, without decoding the pixels.
/// For RAW this is the sensor size (before the develop crop, so slightly larger).
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let (w, h, o) = match kind_of(path)? {
        Kind::Raster => {
            let mut dec = ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?
                .into_decoder()
                .ok()?;
            let o = dec.orientation().ok().map_or(1, |o| o.to_exif());
            let (w, h) = dec.dimensions();
            (w, h, o)
        }
        Kind::Raw => {
            use rawler::decoders::RawDecodeParams;
            let src = rawler::rawsource::RawSource::new(path).ok()?;
            let decoder = rawler::get_decoder(&src).ok()?;
            let raw = decoder
                .raw_image(&src, &RawDecodeParams::default(), true)
                .ok()?;
            let (w, h) = raw
                .crop_area
                .map_or((raw.width, raw.height), |c| (c.d.w, c.d.h));
            (w as u32, h as u32, raw::orientation(path) as u8)
        }
        Kind::Other | Kind::Video => return None,
    };
    // 5 to 8 swap width and height.
    Some(if (5..=8).contains(&o) { (h, w) } else { (w, h) })
}

/// Decodes image data from memory, such as a picture from the server, and
/// applies EXIF orientation (originals from phones often carry Orientation 6).
pub fn load_bytes(bytes: &[u8]) -> Result<Pixels> {
    let e = |e| CoreError::image("<speicher>", e);
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| CoreError::io("<speicher>", err))?
        .into_decoder()
        .map_err(e)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(e)?;
    img.apply_orientation(orientation);
    Ok(Pixels::from_dynamic(img))
}

/// Loads for display: decode, orient, shrink to the target size.
/// For RAW the embedded preview is enough if it's big enough.
pub fn load_preview(path: &Path, max_w: u32, max_h: u32) -> Result<Pixels> {
    let img = match kind_of(path) {
        Some(Kind::Raw) => raw::load_preview(path, max_w.max(max_h).min(RAW_PREVIEW_MIN))?,
        Some(Kind::Video) => video_frame(path, max_w.max(max_h))?,
        _ => load_oriented(path)?,
    };
    Pixels::from_dynamic(img).fit_within(max_w, max_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_size_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_size(4000, 3000, 400, 400), (400, 300));
        assert_eq!(fit_size(3000, 4000, 400, 400), (300, 400));
        assert_eq!(fit_size(100, 50, 400, 400), (100, 50));
        assert_eq!(fit_size(10000, 1, 100, 100), (100, 1));
    }

    #[test]
    fn bytes_follow_exif_orientation() {
        use img_parts::jpeg::Jpeg;
        use img_parts::{Bytes, ImageEXIF};
        let img = DynamicImage::ImageRgb8(image::RgbImage::new(4, 2));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        let plain = load_bytes(buf.get_ref()).unwrap();
        assert_eq!((plain.width, plain.height), (4, 2));
        let mut jpeg = Jpeg::from_bytes(Bytes::from(buf.into_inner())).unwrap();
        jpeg.set_exif(Some(Bytes::from(crate::exif::minimal_exif(6))));
        let turned = load_bytes(&jpeg.encoder().bytes()).unwrap();
        assert_eq!((turned.width, turned.height), (2, 4));
    }

    #[test]
    fn fit_within_scales_pixels() {
        let px = Pixels {
            width: 8,
            height: 4,
            data: vec![200; 8 * 4 * 4],
        };
        let small = px.fit_within(4, 4).unwrap();
        assert_eq!((small.width, small.height), (4, 2));
        assert_eq!(small.data.len(), 4 * 2 * 4);
        assert!(small.data.iter().all(|&v| v == 200));
    }
}
