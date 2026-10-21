//! glycin als Rückfall-Dekodierer für den Kern: HEIC, AVIF, JPEG XL, SVG, EXR
//! und was die Laufzeit sonst an Loadern mitbringt.
//!
//! glycin dekodiert in einem abgeschotteten Prozess. Eine kaputte oder
//! bösartige Datei kann so höchstens den Loader abschießen, nicht Blick.
//! Ohne installierte Loader (etwa auf dem Host) scheitert das Laden mit einer
//! Meldung, und das Bild erscheint als defekt.

use std::path::Path;

use blick_core::Rotation;
use blick_core::{CoreError, Result};
use glycin::{Loader, MainContextSelector, MemoryFormatSelection};
use gtk::gio;
use image::{DynamicImage, RgbaImage};

use crate::runtime;

fn err(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Decode {
        path: path.to_owned(),
        reason: e.to_string(),
    }
}

/// Lädt das erste Bild der Datei als RGBA8, ausgerichtet und nach sRGB gewandelt.
/// Wird aus Worker-Threads aufgerufen, nie aus dem UI-Thread oder der tokio-Runtime.
pub fn load_blocking(path: &Path) -> Result<DynamicImage> {
    let file = gio::File::for_path(path);
    let frame = runtime::runtime()
        .block_on(async move {
            let mut loader = Loader::new(file);
            loader.accepted_memory_formats(MemoryFormatSelection::R8g8b8a8);
            // Eigener glycin-Thread statt des GTK-Hauptloops: dort läuft keine
            // tokio-Runtime, und der UI-Thread soll ohnehin frei bleiben.
            loader.main_context_selector(MainContextSelector::Managed);
            // Selbst ausrichten: Eine Drehung im Sidecar soll die der Datei ersetzen.
            loader.apply_transformations(false);
            let mut image = loader.load().await?;
            let orientation = image.transformation_orientation() as u16;
            Ok::<_, glycin::Error>((image.next_frame().await?, orientation))
        })
        .map_err(|e| err(path, e))?;
    let (frame, file_orientation) = frame;

    let (w, h, stride) = (frame.width(), frame.height(), frame.stride() as usize);
    let src = frame.buf_slice();
    let row = w as usize * 4;
    let mut data = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        data.extend_from_slice(&src[y * stride..y * stride + row]);
    }
    let img = RgbaImage::from_raw(w, h, data).ok_or_else(|| err(path, "Puffer zu klein"))?;
    let mut img = DynamicImage::ImageRgba8(img);
    let o = blick_core::sidecar::read_orientation(path).unwrap_or(file_orientation);
    if let Some(o) = image::metadata::Orientation::from_exif(o as u8) {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Ausrichtung laut Datei, ohne die Pixel zu dekodieren.
fn file_orientation(path: &Path) -> Result<u16> {
    let file = gio::File::for_path(path);
    runtime::runtime()
        .block_on(async move {
            let mut loader = Loader::new(file);
            loader.main_context_selector(MainContextSelector::Managed);
            loader.apply_transformations(false);
            Ok::<_, glycin::Error>(loader.load().await?.transformation_orientation() as u16)
        })
        .map_err(|e| err(path, e))
}

/// Dreht HEIC, AVIF, JPEG XL & Co. verlustfrei über das XMP-Sidecar, wie RAW.
/// Neu kodieren hieße Qualität und EXIF verlieren: Der glycin-Creator der
/// Laufzeit kann keine EXIF-Daten schreiben, und Handyfotos leben von ihrem
/// Aufnahmedatum.
pub fn rotate_blocking(path: &Path, rot: Rotation) -> Result<()> {
    let current = match blick_core::sidecar::read_orientation(path) {
        Some(o) => o,
        None => file_orientation(path)?,
    };
    blick_core::sidecar::write_orientation(path, blick_core::exif::rotate_orientation(current, rot))
}
