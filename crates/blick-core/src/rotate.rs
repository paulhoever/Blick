//! Drehen um 90°.
//!
//! JPEG: nur das EXIF-Orientation-Tag wird umgeschrieben, die Bilddaten
//! bleiben bitgleich. Alles andere wird gedreht und neu kodiert.

use std::path::Path;

use img_parts::jpeg::Jpeg;
use img_parts::{Bytes, ImageEXIF};

use crate::decode::load_oriented;
use crate::encode::{carry_metadata, encode, format_of, write_atomic};
use crate::error::{CoreError, Result};
use crate::exif;
use crate::formats::{Kind, kind_of};
use crate::{raw, sidecar};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Left,
    Right,
}

impl Rotation {
    pub fn inverse(self) -> Self {
        match self {
            Rotation::Left => Rotation::Right,
            Rotation::Right => Rotation::Left,
        }
    }
}

/// Dreher für Formate, die der Kern nur über die App lesen kann.
pub type Fallback = dyn Fn(&Path, Rotation) -> Result<()> + Send + Sync;

static FALLBACK: std::sync::OnceLock<Box<Fallback>> = std::sync::OnceLock::new();

pub fn set_fallback(f: impl Fn(&Path, Rotation) -> Result<()> + Send + Sync + 'static) {
    let _ = FALLBACK.set(Box::new(f));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Nur Metadaten geändert, verlustfrei.
    Exif,
    /// Pixel gedreht und neu kodiert.
    Pixels,
    /// RAW, HEIC & Co.: Drehung im XMP-Sidecar, die Datei bleibt unangetastet.
    Sidecar,
}

/// Dreht die Datei und zieht einen gemerkten Zuschnitt mit, sonst säße er
/// danach an der falschen Stelle.
pub fn rotate_file(path: &Path, rot: Rotation) -> Result<Method> {
    let method = rotate_pixels_or_tag(path, rot)?;
    sidecar::rotate_crop(path, rot)?;
    Ok(method)
}

fn rotate_pixels_or_tag(path: &Path, rot: Rotation) -> Result<Method> {
    match kind_of(path) {
        Some(Kind::Raw) => {
            let next = exif::rotate_orientation(raw::orientation(path), rot);
            sidecar::write_orientation(path, next)?;
            return Ok(Method::Sidecar);
        }
        // HEIC, AVIF & Co.: über den registrierten Dreher (in der App: Sidecar, Ausrichtung per glycin).
        Some(Kind::Video) => return Err(CoreError::Unsupported(path.to_owned())),
        Some(Kind::Other) => {
            let f = FALLBACK
                .get()
                .ok_or_else(|| CoreError::Unsupported(path.to_owned()))?;
            f(path, rot)?;
            return Ok(Method::Sidecar);
        }
        _ => {}
    }
    let original = std::fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let format = format_of(path)?;

    if format == image::ImageFormat::Jpeg
        && let Some(bytes) = rotate_jpeg_exif(&original, rot)
    {
        write_atomic(path, &bytes)?;
        return Ok(Method::Exif);
    }

    let img = load_oriented(path)?;
    let turned = match rot {
        Rotation::Left => img.rotate270(),
        Rotation::Right => img.rotate90(),
    };
    let encoded = encode(&turned, format, path)?;
    write_atomic(path, &carry_metadata(&original, encoded))?;
    Ok(Method::Pixels)
}

/// `None`, wenn es zwar EXIF gibt, das Tag aber fehlt. Dann geht es über die Pixel.
fn rotate_jpeg_exif(original: &[u8], rot: Rotation) -> Option<Vec<u8>> {
    let mut jpeg = Jpeg::from_bytes(Bytes::copy_from_slice(original)).ok()?;
    let block = match jpeg.exif() {
        Some(existing) => {
            let mut data = existing.to_vec();
            let current = exif::read_orientation(&data)?;
            if !exif::write_orientation(&mut data, exif::rotate_orientation(current, rot)) {
                return None;
            }
            data
        }
        None => exif::minimal_exif(exif::rotate_orientation(1, rot)),
    };
    jpeg.set_exif(Some(Bytes::from(block)));
    Some(jpeg.encoder().bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};

    fn gradient(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([x as u8 * 20, y as u8 * 30, 7])
        }))
    }

    fn orientation_of(path: &Path) -> image::metadata::Orientation {
        let mut d = ImageReader::open(path).unwrap().into_decoder().unwrap();
        d.orientation().unwrap()
    }

    #[test]
    fn jpeg_rotates_losslessly_via_exif() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        gradient(6, 4).save(&p).unwrap();
        let before = std::fs::read(&p).unwrap();
        let scan_before = Jpeg::from_bytes(Bytes::from(before)).unwrap();

        assert_eq!(rotate_file(&p, Rotation::Right).unwrap(), Method::Exif);
        assert_eq!(orientation_of(&p), image::metadata::Orientation::Rotate90);
        let img = load_oriented(&p).unwrap();
        assert_eq!((img.width(), img.height()), (4, 6));

        // Die komprimierten Bilddaten sind unverändert.
        let after = Jpeg::from_bytes(Bytes::from(std::fs::read(&p).unwrap())).unwrap();
        let sos = |j: &Jpeg| {
            j.segments()
                .iter()
                .filter(|s| s.marker() != img_parts::jpeg::markers::APP1)
                .map(|s| s.contents().clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(sos(&scan_before), sos(&after));

        for _ in 0..3 {
            rotate_file(&p, Rotation::Right).unwrap();
        }
        assert_eq!(
            orientation_of(&p),
            image::metadata::Orientation::NoTransforms
        );
    }

    #[test]
    fn png_four_turns_are_pixel_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        let original = gradient(5, 3);
        original.save(&p).unwrap();
        for _ in 0..4 {
            assert_eq!(rotate_file(&p, Rotation::Left).unwrap(), Method::Pixels);
        }
        assert_eq!(image::open(&p).unwrap().to_rgb8(), original.to_rgb8());
    }

    #[test]
    fn png_keeps_exif_and_resets_orientation() {
        use img_parts::png::Png;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        gradient(4, 2).save(&p).unwrap();
        let mut png = Png::from_bytes(Bytes::from(std::fs::read(&p).unwrap())).unwrap();
        png.set_exif(Some(Bytes::from(exif::minimal_exif(6))));
        std::fs::write(&p, png.encoder().bytes()).unwrap();

        rotate_file(&p, Rotation::Right).unwrap();
        let png = Png::from_bytes(Bytes::from(std::fs::read(&p).unwrap())).unwrap();
        assert_eq!(exif::read_orientation(&png.exif().unwrap()), Some(1));
    }

    #[test]
    fn remembered_crop_turns_with_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        gradient(4, 2).save(&p).unwrap();
        let crop = crate::Crop {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        crate::sidecar::write_edit(
            &p,
            &crate::sidecar::EditSettings {
                look_strength: 1.0,
                crop: Some(crop),
                ..Default::default()
            },
        )
        .unwrap();
        let before = crop.apply(&crate::Pixels::from_dynamic(load_oriented(&p).unwrap()));
        rotate_file(&p, Rotation::Right).unwrap();
        let turned = crate::sidecar::read_edit(&p).unwrap().crop.unwrap();
        let after = turned.apply(&crate::Pixels::from_dynamic(load_oriented(&p).unwrap()));
        let expect = crate::Pixels::from_dynamic(before.into_dynamic().rotate90());
        assert_eq!(after.data, expect.data);
    }

    #[test]
    fn raw_rotates_via_sidecar_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.CR3");
        std::fs::write(&p, b"kein echtes raw").unwrap();
        assert_eq!(rotate_file(&p, Rotation::Right).unwrap(), Method::Sidecar);
        assert_eq!(crate::sidecar::read_orientation(&p), Some(6));
        assert_eq!(rotate_file(&p, Rotation::Right).unwrap(), Method::Sidecar);
        assert_eq!(crate::sidecar::read_orientation(&p), Some(3));
        assert_eq!(std::fs::read(&p).unwrap(), b"kein echtes raw");
    }

    #[test]
    fn file_permissions_survive() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        gradient(4, 4).save(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        rotate_file(&p, Rotation::Left).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
