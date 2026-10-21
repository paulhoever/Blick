//! Dekodieren mit EXIF-Orientierung und schnellem Herunterskalieren.

use std::path::Path;
use std::sync::OnceLock;

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::error::{CoreError, Result};
use crate::formats::{Kind, kind_of};
use crate::raw;

/// RGBA8-Pixelpuffer, dicht gepackt (stride = width * 4).
/// Das Format passt 1:1 auf `gdk::MemoryFormat::R8g8b8a8`.
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
            .expect("Pixels hat immer width*height*4 Bytes");
        DynamicImage::ImageRgba8(buf)
    }

    /// Verkleinert so, dass das Bild in `max_w` x `max_h` passt. Vergrößert nie.
    pub fn fit_within(self, max_w: u32, max_h: u32) -> Result<Self> {
        let (w, h) = fit_size(self.width, self.height, max_w, max_h);
        if (w, h) == (self.width, self.height) {
            return Ok(self);
        }
        let src = FirImage::from_vec_u8(self.width, self.height, self.data, PixelType::U8x4)
            .map_err(|e| CoreError::Resize(e.to_string()))?;
        let mut dst = FirImage::new(w, h, PixelType::U8x4);
        // Bilinear reicht für die Anzeige und ist deutlich schneller als Lanczos.
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

/// Zielgröße mit erhaltenem Seitenverhältnis, mindestens 1x1.
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

/// Dekodierer für alles, was weder `image` noch `rawler` können. Die App
/// registriert hier glycin; ohne Registrierung gelten solche Dateien als
/// nicht unterstützt.
pub type Fallback = dyn Fn(&Path) -> Result<DynamicImage> + Send + Sync;

static FALLBACK: OnceLock<Box<Fallback>> = OnceLock::new();

pub fn set_fallback(f: impl Fn(&Path) -> Result<DynamicImage> + Send + Sync + 'static) {
    let _ = FALLBACK.set(Box::new(f));
}

/// Holt ein Standbild aus einem Video. Die App registriert GStreamer; ohne
/// Registrierung haben Videos kein Thumbnail.
pub type FrameGrabber = dyn Fn(&Path, u32) -> Result<DynamicImage> + Send + Sync;

static FRAME_GRABBER: OnceLock<Box<FrameGrabber>> = OnceLock::new();

pub fn set_frame_grabber(f: impl Fn(&Path, u32) -> Result<DynamicImage> + Send + Sync + 'static) {
    let _ = FRAME_GRABBER.set(Box::new(f));
}

/// Standbild aus dem Video, höchstens `max_edge` Pixel an der längeren Kante.
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

/// Größte Kantenlänge, ab der eine eingebettete RAW-Vorschau reicht.
const RAW_PREVIEW_MIN: u32 = 1024;

/// Dekodiert vollständig und richtet nach EXIF-Orientierung aus.
/// RAW wird dabei voll entwickelt; das braucht Export und Bearbeiten.
pub fn load_oriented(path: &Path) -> Result<DynamicImage> {
    match kind_of(path) {
        Some(Kind::Raw) => raw::develop(path),
        Some(Kind::Other) => fallback(path),
        Some(Kind::Video) => video_frame(path, 4096),
        // Was `image` nicht schafft (exotisches TIFF etwa), bekommt glycin.
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
    // Fehlende oder kaputte EXIF-Daten sind kein Grund, das Bild nicht zu zeigen.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| CoreError::image(path, e))?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Maße des Originals nach Ausrichtung, ohne die Pixel zu dekodieren.
/// Bei RAW die Sensorgröße (vor dem Beschnitt der Entwicklung, also knapp darüber).
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
    // 5 bis 8 vertauschen Breite und Höhe.
    Some(if (5..=8).contains(&o) { (h, w) } else { (w, h) })
}

/// Dekodiert Bilddaten aus dem Speicher, etwa ein Bild vom Server, und
/// richtet sie nach EXIF aus (Originale vom Handy tragen oft Orientation 6).
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

/// Lädt für die Anzeige: dekodieren, ausrichten, auf die Zielgröße verkleinern.
/// Bei RAW reicht die eingebettete Vorschau, sofern sie groß genug ist.
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
