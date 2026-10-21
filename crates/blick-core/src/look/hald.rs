//! HaldCLUT-PNGs, wie die Film Simulation Collection von RawTherapee.
//!
//! Eine HaldCLUT der Stufe L ist ein quadratisches Bild mit L³ × L³ Pixeln.
//! Pixel Nummer i (zeilenweise) ist der Ausgabewert für den Gitterpunkt i
//! eines Würfels mit L² Punkten je Achse, Rot läuft am schnellsten.

use super::lut::Lut3d;

/// Gittergröße, auf die große HaldCLUTs heruntergerechnet werden. 144³ (Stufe 12)
/// kostet 36 MB und ist von 64³ nicht zu unterscheiden.
const MAX_SIZE: usize = 64;

pub fn from_image(img: &image::DynamicImage) -> Result<Lut3d, String> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w != h {
        return Err(format!("HaldCLUT muss quadratisch sein, ist {w}x{h}"));
    }
    let level = (2..=16)
        .find(|l| l * l * l == w)
        .ok_or_else(|| format!("{w}x{w} ist keine HaldCLUT-Größe (512, 729, 1000, 1728 …)"))?;
    let size = level * level;
    let rgb = img.to_rgb32f();
    let data = rgb.pixels().map(|p| [p[0], p[1], p[2]]).collect();
    let lut = Lut3d { size, data };
    Ok(if size > MAX_SIZE {
        lut.resampled(MAX_SIZE)
    } else {
        lut
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Erzeugt die Identitäts-HaldCLUT einer Stufe als Bild.
    pub fn identity_image(level: u32) -> image::DynamicImage {
        let size = level * level;
        let side = level * level * level;
        let img = image::RgbImage::from_fn(side, side, |x, y| {
            let i = y * side + x;
            let (r, g, b) = (i % size, (i / size) % size, i / (size * size));
            let s = |v: u32| (v as f32 / (size - 1) as f32 * 255.0).round() as u8;
            image::Rgb([s(r), s(g), s(b)])
        });
        image::DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn identity_level_8() {
        let lut = from_image(&identity_image(8)).unwrap();
        assert_eq!(lut.size, 64);
        let o = lut.sample([0.3, 0.6, 0.9]);
        assert!(
            (o[0] - 0.3).abs() < 0.01 && (o[1] - 0.6).abs() < 0.01 && (o[2] - 0.9).abs() < 0.01,
            "{o:?}"
        );
    }

    #[test]
    fn rejects_odd_sizes() {
        assert!(from_image(&image::DynamicImage::new_rgb8(500, 500)).is_err());
        assert!(from_image(&image::DynamicImage::new_rgb8(512, 256)).is_err());
    }
}
