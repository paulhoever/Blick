//! Kamera-RAW über rawler.
//!
//! Zum Ansehen reicht fast immer die JPEG-Vorschau, die die Kamera in die Datei
//! legt: oft in voller Auflösung und in einem Bruchteil der Zeit einer echten
//! Entwicklung. Entwickelt wird nur, wenn es keine Vorschau gibt oder für den
//! Export in voller Qualität.

use std::path::Path;

use image::DynamicImage;
use image::metadata::Orientation;
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::error::{CoreError, Result};
use crate::sidecar;

fn err(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Decode {
        path: path.to_owned(),
        reason: e.to_string(),
    }
}

/// Drehung: erst der Sidecar (dort legt Blick sie ab), sonst EXIF der Kamera.
pub fn orientation(path: &Path) -> u16 {
    if let Some(o) = sidecar::read_orientation(path) {
        return o;
    }
    let read = || -> Option<u16> {
        let src = RawSource::new(path).ok()?;
        let dec = rawler::get_decoder(&src).ok()?;
        dec.raw_metadata(&src, &RawDecodeParams::default())
            .ok()?
            .exif
            .orientation
    };
    read().filter(|o| (1..=8).contains(o)).unwrap_or(1)
}

fn oriented(mut img: DynamicImage, o: u16) -> DynamicImage {
    if let Some(o) = Orientation::from_exif(o as u8) {
        img.apply_orientation(o);
    }
    img
}

/// Eingebettete Vorschau, ausgerichtet. Kleine Vorschauen (unter `min_px`)
/// werden übergangen und es wird entwickelt.
pub fn load_preview(path: &Path, min_px: u32) -> Result<DynamicImage> {
    let params = RawDecodeParams::default();
    let img = rawler::analyze::extract_preview_pixels(path, &params).map_err(|e| err(path, e))?;
    let img = if img.width().max(img.height()) < min_px {
        develop_unoriented(path)?
    } else {
        img
    };
    Ok(oriented(img, orientation(path)))
}

fn develop_unoriented(path: &Path) -> Result<DynamicImage> {
    rawler::analyze::raw_to_srgb(path, &RawDecodeParams::default()).map_err(|e| err(path, e))
}

/// Volle Entwicklung (Demosaic, Weißabgleich der Kamera, sRGB), ausgerichtet.
/// Tonwerte und Farbbalance werden an die eingebettete Kamera-Vorschau
/// angeglichen, sonst wirkt die Entwicklung deutlich flacher als das, was die
/// Kamera und jeder Betrachter zeigt.
pub fn develop(path: &Path) -> Result<DynamicImage> {
    let dev = develop_unoriented(path)?;
    let preview = rawler::analyze::extract_preview_pixels(path, &RawDecodeParams::default()).ok();
    let matched = match preview {
        Some(p) => match_tones(&dev, &p).unwrap_or(dev),
        None => dev,
    };
    Ok(oriented(matched, orientation(path)))
}

/// Feinheit der Tonwert-Tabelle für 16-Bit-Eingaben.
const BINS: usize = 4096;
/// Kantenlänge, auf die beide Bilder für den Vergleich verkleinert werden.
const SAMPLE_PX: u32 = 640;

/// Histogramm-Abgleich pro Kanal: Jeder Wert der Entwicklung bekommt den Wert,
/// der in der Vorschau am selben Perzentil liegt. Monoton, also ohne
/// Tonwertsprünge. `None`, wenn die Bilder nicht dasselbe zeigen (anderes
/// Seitenverhältnis, etwa bei beschnittener Vorschau).
pub fn match_tones(dev: &DynamicImage, preview: &DynamicImage) -> Option<DynamicImage> {
    let ra = dev.width() as f32 / dev.height() as f32;
    let rb = preview.width() as f32 / preview.height() as f32;
    if (ra - rb).abs() / ra > 0.03 {
        return None;
    }
    let small =
        |img: &DynamicImage| img.thumbnail_exact(SAMPLE_PX, (SAMPLE_PX as f32 / ra).round() as u32);
    let d = small(dev).to_rgb16();
    let p = small(preview).to_rgb8();

    let mut luts = [[0u8; BINS]; 3];
    for (c, lut) in luts.iter_mut().enumerate() {
        let mut hd = vec![0u32; BINS];
        let mut hp = [0u32; 256];
        for px in d.pixels() {
            hd[px[c] as usize * BINS / 65536] += 1;
        }
        for px in p.pixels() {
            hp[px[c] as usize] += 1;
        }
        let (nd, np) = (d.pixels().len() as f64, p.pixels().len() as f64);
        // Kumulierte Verteilung der Vorschau, dann für jedes Bin der Entwicklung
        // den Vorschauwert am selben Perzentil (Mitte des Bins).
        let mut cp = [0f64; 256];
        let mut acc = 0.0;
        for (i, v) in hp.iter().enumerate() {
            acc += *v as f64 / np;
            cp[i] = acc;
        }
        let mut acc = 0.0;
        let mut j = 0usize;
        for (i, v) in hd.iter().enumerate() {
            let q = acc + *v as f64 / nd / 2.0;
            acc += *v as f64 / nd;
            while j < 255 && cp[j] < q {
                j += 1;
            }
            lut[i] = j as u8;
        }
    }
    let full = dev.to_rgb16();
    let (w, h) = full.dimensions();
    let mut out = image::RgbImage::new(w, h);
    for (o, i) in out.pixels_mut().zip(full.pixels()) {
        for c in 0..3 {
            o[c] = luts[c][i[c] as usize * BINS / 65536];
        }
    }
    Some(DynamicImage::ImageRgb8(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echte RAWs sind zu groß fürs Repo. `BLICK_RAW_SAMPLES=<ordner>` zeigt auf
    /// Muster von raw.pixls.us; ohne die Variable wird der Test übersprungen.
    #[test]
    fn matching_pulls_dev_towards_preview() {
        // „Entwicklung“ flach und grau, „Vorschau“ mit Kontrast und Farbstich.
        let dev = DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(64, 32, |x, _| {
            let v = (20000 + x * 300) as u16;
            image::Rgb([v, v, v])
        }));
        let prev = DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 32, |x, _| {
            let v = (x * 4) as u8;
            image::Rgb([v.saturating_add(10), v, v / 2])
        }));
        let m = match_tones(&dev, &prev).unwrap().to_rgb8();
        let (dark, bright) = (m.get_pixel(0, 0), m.get_pixel(63, 0));
        assert!(dark[0] < 30 && bright[0] > 200, "{dark:?} {bright:?}");
        assert!(bright[2] < bright[0], "Farbstich übernommen");
        // Anderes Seitenverhältnis: kein Abgleich.
        assert!(match_tones(&dev, &DynamicImage::new_rgb8(32, 32)).is_none());
    }

    #[test]
    fn samples_decode() {
        let Ok(dir) = std::env::var("BLICK_RAW_SAMPLES") else {
            return;
        };
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if crate::formats::kind_of(&p) != Some(crate::formats::Kind::Raw) {
                continue;
            }
            let t = std::time::Instant::now();
            let img = load_preview(&p, 1024).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            println!(
                "{}: {}x{} in {:?}",
                p.display(),
                img.width(),
                img.height(),
                t.elapsed()
            );
            assert!(img.width() >= 1024 || img.height() >= 1024);
        }
    }
}
