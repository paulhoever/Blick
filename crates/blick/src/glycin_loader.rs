//! glycin as the fallback decoder for the core: HEIC, AVIF, JPEG XL, SVG, EXR
//! and whatever else loaders the runtime brings along.
//!
//! glycin decodes in a sandboxed process. A broken or
//! malicious file can at most kill the loader, not Blick.
//! Without installed loaders (on the host, say) loading fails with a
//! message and the picture shows up as broken.

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

/// Loads the first image of the file as RGBA8, oriented and converted to sRGB.
/// Called from worker threads, never from the UI thread or the tokio runtime.
pub fn load_blocking(path: &Path) -> Result<DynamicImage> {
    let file = gio::File::for_path(path);
    let frame = runtime::runtime()
        .block_on(async move {
            let mut loader = Loader::new(file);
            loader.accepted_memory_formats(MemoryFormatSelection::R8g8b8a8);
            // Own glycin thread instead of the GTK main loop: no tokio runtime
            // runs there, and the UI thread should stay free anyway.
            loader.main_context_selector(MainContextSelector::Managed);
            // Orient ourselves: a rotation in the sidecar should replace the file's own.
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
    let img = RgbaImage::from_raw(w, h, data).ok_or_else(|| err(path, "buffer too small"))?;
    let mut img = DynamicImage::ImageRgba8(img);
    let o = blick_core::sidecar::read_orientation(path).unwrap_or(file_orientation);
    if let Some(o) = image::metadata::Orientation::from_exif(o as u8) {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Orientation according to the file, without decoding the pixels.
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

/// Rotates HEIC, AVIF, JPEG XL and the like losslessly through the XMP sidecar, like RAW.
/// Re-encoding would lose quality and EXIF: the runtime's glycin creator
/// can't write EXIF data, and phone photos depend on their capture date.
pub fn rotate_blocking(path: &Path, rot: Rotation) -> Result<()> {
    let current = match blick_core::sidecar::read_orientation(path) {
        Some(o) => o,
        None => file_orientation(path)?,
    };
    blick_core::sidecar::write_orientation(path, blick_core::exif::rotate_orientation(current, rot))
}
