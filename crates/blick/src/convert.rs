//! Ordner nach JPEG XL umwandeln, um Platz zu sparen.
//!
//! - JPEG: verlustfreie Umpackung (Rekonstruktion bitgenau, rund 20 % kleiner).
//! - PNG: verlustfrei.
//! - WebP, HEIC/HEIF: sichtbar verlustfrei (Distanz 1,0), immer umgewandelt, weil viele
//!   Programme diese Formate nicht lesen; die Datei darf dabei größer werden.
//!
//! EXIF wandert mit. Das Original wird erst angefasst, wenn die neue Datei
//! geschrieben und gegengelesen ist.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blick_core::CoreError;
use gtk::gio;
use gtk::prelude::*;
use image::DynamicImage;
use jpegxl_rs::decode::Data;
use jpegxl_rs::encode::{EncoderFrame, Metadata};
use jpegxl_rs::{decoder_builder, encoder_builder};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Jpeg,
    Png,
    Webp,
    Heic,
}

fn kind(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some(Kind::Jpeg),
        "png" => Some(Kind::Png),
        "webp" => Some(Kind::Webp),
        "heic" | "heif" => Some(Kind::Heic),
        _ => None,
    }
}

/// Alle umwandelbaren Bilder unter `dir`, rekursiv. Punkt-Ordner bleiben außen vor.
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            let Ok(kind_of_entry) = entry.file_type() else {
                continue;
            };
            if hidden {
                continue;
            }
            if kind_of_entry.is_dir() {
                stack.push(path);
            } else if kind_of_entry.is_file() && kind(&path).is_some() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Zählung für die Rückfrage: JPEG, PNG, WebP, HEIC.
pub fn count_by_kind(paths: &[PathBuf]) -> [usize; 4] {
    let mut n = [0; 4];
    for p in paths {
        match kind(p) {
            Some(Kind::Jpeg) => n[0] += 1,
            Some(Kind::Png) => n[1] += 1,
            Some(Kind::Webp) => n[2] += 1,
            Some(Kind::Heic) => n[3] += 1,
            None => {}
        }
    }
    n
}

fn fail(path: &Path, reason: impl std::fmt::Display) -> CoreError {
    CoreError::Decode {
        path: path.to_owned(),
        reason: reason.to_string(),
    }
}

/// Einen Eintrag Pixel für den Encoder.
enum Raster {
    Eight { data: Vec<u8>, channels: u32 },
    Sixteen { data: Vec<u16>, channels: u32 },
}

/// Wandelt eine Datei um. `saved` zählt die gesparten Bytes über alle Dateien.
/// `trash_original`: Original danach in den Papierkorb (sonst bleibt es liegen).
pub fn convert_file(path: &Path, trash_original: bool, saved: &AtomicU64) -> blick_core::Result<()> {
    let kind = kind(path).ok_or_else(|| fail(path, "Format wird nicht umgewandelt"))?;
    let target = path.with_extension("jxl");
    if target.exists() {
        return Err(fail(path, "es gibt schon eine .jxl daneben"));
    }
    if blick_core::sidecar::path_for(path).exists() {
        return Err(fail(path, "hat eine Bearbeitung (Sidecar), bleibt unberührt"));
    }
    let src = std::fs::read(path).map_err(|e| fail(path, e))?;
    let jxl = match kind {
        Kind::Jpeg => encode_jpeg(path, &src)?,
        Kind::Png => encode_png(path, &src)?,
        Kind::Webp => encode_lossy(path, &src, kind)?,
        Kind::Heic => encode_lossy(path, &src, kind)?,
    };
    // WebP und HEIC sollen immer weg (viele Programme lesen sie nicht), auch wenn JXL
    // größer ausfällt. Bei JPEG und PNG lohnt sich die Umwandlung nur, wenn sie spart.
    let must_convert = matches!(kind, Kind::Webp | Kind::Heic);
    if !must_convert && jxl.len() >= src.len() {
        return Err(fail(path, "als JPEG XL nicht kleiner"));
    }

    // Erst daneben schreiben, dann umbenennen: nie eine halbe .jxl im Ordner.
    let part = target.with_extension("jxl.part");
    std::fs::write(&part, &jxl).map_err(|e| fail(&part, e))?;
    if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified())
        && let Ok(f) = std::fs::File::options().write(true).open(&part)
    {
        let _ = f.set_modified(modified);
    }
    std::fs::rename(&part, &target).map_err(|e| fail(&target, e))?;

    // Größere Ergebnisse (WebP/HEIC) zählen nicht negativ.
    saved.fetch_add((src.len() as u64).saturating_sub(jxl.len() as u64), Ordering::Relaxed);
    if trash_original {
        gio::File::for_path(path)
            .trash(gio::Cancellable::NONE)
            .map_err(|e| fail(path, format!("Original nicht in den Papierkorb: {e}")))?;
    }
    Ok(())
}

fn encode_jpeg(path: &Path, src: &[u8]) -> blick_core::Result<Vec<u8>> {
    let mut enc = encoder_builder()
        .uses_original_profile(true)
        .use_container(true)
        .build()
        .map_err(|e| fail(path, e))?;
    let out = enc.encode_jpeg(src).map_err(|e| fail(path, e))?;
    // Beweis, dass es umkehrbar ist: das JPEG muss bitgleich zurückkommen.
    let dec = decoder_builder().build().map_err(|e| fail(path, e))?;
    match dec.reconstruct(&out).map_err(|e| fail(path, e))?.1 {
        Data::Jpeg(back) if back == src => Ok(out),
        _ => Err(fail(path, "Rückprobe war nicht bitgleich, bleibt JPEG")),
    }
}

fn has_marker(src: &[u8], marker: &[u8], within: usize) -> bool {
    src[..src.len().min(within)]
        .windows(marker.len())
        .any(|w| w == marker)
}

fn exif_tiff(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let mut tiff = exif.buf().to_vec();
    // Wir übergeben ausgerichtete Pixel, die Orientierung steht danach auf „normal“.
    blick_core::exif::write_orientation(&mut tiff, 1);
    let mut boxed = vec![0u8; 4];
    boxed.append(&mut tiff);
    Some(boxed)
}

fn encode_png(path: &Path, src: &[u8]) -> blick_core::Result<Vec<u8>> {
    if has_marker(src, b"acTL", 65536) {
        return Err(fail(path, "animiertes PNG"));
    }
    if has_marker(src, b"iCCP", 65536) {
        return Err(fail(path, "eigenes Farbprofil, bleibt PNG"));
    }
    let img = image::load_from_memory_with_format(src, image::ImageFormat::Png)
        .map_err(|e| fail(path, e))?;
    let (w, h) = (img.width(), img.height());
    let raster = match img {
        DynamicImage::ImageLuma8(_) | DynamicImage::ImageRgb8(_) => Raster::Eight {
            data: img.into_rgb8().into_raw(),
            channels: 3,
        },
        DynamicImage::ImageLumaA8(_) | DynamicImage::ImageRgba8(_) => Raster::Eight {
            data: img.into_rgba8().into_raw(),
            channels: 4,
        },
        DynamicImage::ImageLuma16(_) | DynamicImage::ImageRgb16(_) => Raster::Sixteen {
            data: img.into_rgb16().into_raw(),
            channels: 3,
        },
        DynamicImage::ImageLumaA16(_) | DynamicImage::ImageRgba16(_) => Raster::Sixteen {
            data: img.into_rgba16().into_raw(),
            channels: 4,
        },
        _ => return Err(fail(path, "Pixelformat wird nicht umgewandelt")),
    };
    encode_raster(path, w, h, &raster, true, exif_tiff(path))
}

fn encode_lossy(path: &Path, src: &[u8], kind: Kind) -> blick_core::Result<Vec<u8>> {
    if kind == Kind::Webp {
        let animated = src.get(12..16) == Some(b"VP8X") && src.get(20).is_some_and(|f| f & 2 != 0);
        if animated {
            return Err(fail(path, "animiertes WebP"));
        }
        if has_marker(src, b"ICCP", 4096) {
            return Err(fail(path, "eigenes Farbprofil, bleibt WebP"));
        }
    }
    // Wie in der Anzeige: ausgerichtet, HEIC über glycin.
    let img = blick_core::decode::load_oriented(path)?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_rgba8();
    let opaque = rgba.pixels().all(|p| p.0[3] == 255);
    let raster = if opaque {
        let rgb: Vec<u8> = rgba.pixels().flat_map(|p| [p.0[0], p.0[1], p.0[2]]).collect();
        Raster::Eight { data: rgb, channels: 3 }
    } else {
        Raster::Eight { data: rgba.into_raw(), channels: 4 }
    };
    encode_raster(path, w, h, &raster, false, exif_tiff(path))
}

fn encode_raster(
    path: &Path,
    w: u32,
    h: u32,
    raster: &Raster,
    lossless: bool,
    exif: Option<Vec<u8>>,
) -> blick_core::Result<Vec<u8>> {
    let channels = match raster {
        Raster::Eight { channels, .. } | Raster::Sixteen { channels, .. } => *channels,
    };
    let mut enc = encoder_builder()
        .has_alpha(channels == 4)
        .lossless(lossless)
        .quality(1.0)
        .uses_original_profile(lossless)
        .use_container(true)
        .use_box(true)
        .build()
        .map_err(|e| fail(path, e))?;
    if let Some(buf) = &exif {
        enc.add_metadata(&Metadata::Exif(buf), true);
    }
    let out = match raster {
        Raster::Eight { data, channels } => {
            enc.encode_frame(&EncoderFrame::new(data).num_channels(*channels), w, h)
        }
        Raster::Sixteen { data, channels } => {
            enc.encode_frame(&EncoderFrame::new(data).num_channels(*channels), w, h)
        }
    }
    .map_err(|e| fail(path, e))?;

    // Gegenprobe: lesbar, gleiche Größe; verlustfrei zusätzlich pixelgleich.
    let dec = decoder_builder().build().map_err(|e| fail(path, e))?;
    match raster {
        Raster::Eight { data, .. } => {
            let (meta, back) = dec.decode_with::<u8>(&out).map_err(|e| fail(path, e))?;
            if (meta.width, meta.height) != (w, h) || (lossless && &back != data) {
                return Err(fail(path, "Rückprobe stimmt nicht, bleibt unverändert"));
            }
        }
        Raster::Sixteen { data, .. } => {
            let (meta, back) = dec.decode_with::<u16>(&out).map_err(|e| fail(path, e))?;
            if (meta.width, meta.height) != (w, h) || (lossless && &back != data) {
                return Err(fail(path, "Rückprobe stimmt nicht, bleibt unverändert"));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BLICK_JXL_SAMPLES=<ordner> cargo test -p blick convert -- --nocapture`;
    /// ohne die Variable nichts zu tun. Die Muster liegen nie im Repo.
    #[test]
    fn samples_roundtrip() {
        let Ok(dir) = std::env::var("BLICK_JXL_SAMPLES") else {
            return;
        };
        let files = scan(Path::new(&dir));
        assert!(!files.is_empty());
        let saved = AtomicU64::new(0);
        for f in files {
            let before = std::fs::metadata(&f).unwrap().len();
            match convert_file(&f, false, &saved) {
                Ok(()) => {
                    let after = std::fs::metadata(f.with_extension("jxl")).unwrap().len();
                    println!("{}: {before} -> {after} B", f.display());
                }
                Err(e) => println!("{}: übersprungen: {e}", f.display()),
            }
            assert!(f.exists(), "Original muss bleiben");
        }
        println!("gespart: {} B", saved.load(Ordering::Relaxed));
    }
}
