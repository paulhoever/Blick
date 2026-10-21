//! Save the edited image at full resolution.

use std::path::{Path, PathBuf};

use crate::adjust::{self, Adjustments};
use crate::crop::Crop;
use crate::decode::{Pixels, load_oriented};
use crate::encode::{carry_metadata, encode, format_of, write_atomic};
use crate::error::{CoreError, Result};
use crate::look::Render;

/// Whether Blick can overwrite this file. It can only read RAW, HEIC and the like,
/// and animated GIFs would lose their animation.
pub fn can_overwrite(path: &Path) -> bool {
    crate::formats::kind_of(path) == Some(crate::formats::Kind::Raster)
        && !matches!(
            crate::encode::format_of(path),
            Ok(image::ImageFormat::Gif) | Err(_)
        )
}

/// `photo.jpg` → `photo-edited.jpg`, on a collision `photo-edited-2.jpg` and so on.
pub fn copy_path(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Blick can't write RAW, HEIC and the like, so the copy becomes a JPEG.
    let writable = crate::formats::kind_of(src) == Some(crate::formats::Kind::Raster)
        && !matches!(
            crate::encode::format_of(src),
            Ok(image::ImageFormat::Gif) | Err(_)
        );
    let ext = match src.extension() {
        Some(e) if writable => format!(".{}", e.to_string_lossy()),
        _ => ".jpg".to_owned(),
    };
    let dir = src.parent().unwrap_or(Path::new("."));
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                String::new()
            } else {
                format!("-{n}")
            };
            dir.join(format!("{stem}-bearbeitet{suffix}{ext}"))
        })
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Crops, applies the adjustments to the original and writes to
/// `dst`. `dst == src` overwrites the original (atomically). Crop first,
/// so vignette and grain relate to the new frame.
pub fn save_adjusted(
    src: &Path,
    dst: &Path,
    crop: Option<Crop>,
    adj: Adjustments,
    look: Option<&Render>,
) -> Result<()> {
    let original = std::fs::read(src).map_err(|e| CoreError::io(src, e))?;
    let mut px = Pixels::from_dynamic(load_oriented(src)?);
    if let Some(c) = crop {
        px = c.apply(&px);
    }
    adjust::apply(&mut px.data, adj);
    if let Some(look) = look {
        look.apply(&mut px, crate::look::seed_for(src));
    }
    let encoded = encode(&px.into_dynamic(), format_of(dst)?, dst)?;
    write_atomic(dst, &carry_metadata(&original, encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_path_counts_up() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("foto.jpg");
        assert_eq!(copy_path(&src), d.path().join("foto-bearbeitet.jpg"));
        std::fs::write(d.path().join("foto-bearbeitet.jpg"), b"").unwrap();
        assert_eq!(copy_path(&src), d.path().join("foto-bearbeitet-2.jpg"));
        assert_eq!(
            copy_path(&d.path().join("x.CR3")),
            d.path().join("x-bearbeitet.jpg")
        );
        assert_eq!(
            copy_path(&d.path().join("y.heic")),
            d.path().join("y-bearbeitet.jpg")
        );
    }

    #[test]
    fn saves_adjusted_copy() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.png");
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3,
            3,
            image::Rgb([100, 50, 10]),
        ))
        .save(&src)
        .unwrap();
        let dst = copy_path(&src);
        save_adjusted(
            &src,
            &dst,
            None,
            Adjustments {
                saturation: -1.0,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let out = image::open(&dst).unwrap().to_rgb8();
        let p = out.get_pixel(1, 1);
        assert!(p[0] == p[1] && p[1] == p[2]);
        assert_eq!(image::open(&src).unwrap().to_rgb8().get_pixel(1, 1)[0], 100);
    }

    #[test]
    fn saves_cropped_copy() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("c.png");
        let mut img = image::RgbImage::from_pixel(40, 20, image::Rgb([10, 10, 10]));
        // Right half bright, so you can see which part is left over.
        for x in 20..40 {
            for y in 0..20 {
                img.put_pixel(x, y, image::Rgb([250, 250, 250]));
            }
        }
        image::DynamicImage::ImageRgb8(img).save(&src).unwrap();
        let dst = copy_path(&src);
        let crop = Crop {
            x: 0.5,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        save_adjusted(&src, &dst, Some(crop), Adjustments::default(), None).unwrap();
        let out = image::open(&dst).unwrap().to_rgb8();
        assert_eq!(out.dimensions(), (20, 10));
        assert!(out.pixels().all(|p| p[0] == 250));
        assert_eq!(image::open(&src).unwrap().to_rgb8().dimensions(), (40, 20));
    }
}
