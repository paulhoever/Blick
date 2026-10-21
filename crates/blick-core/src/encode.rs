//! Encoding and safe write-back, metadata is carried over.

use std::io::{Cursor, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageFormat};
use img_parts::{Bytes, DynImage, ImageEXIF, ImageICC};

use crate::error::{CoreError, Result};
use crate::exif;

pub const JPEG_QUALITY: u8 = 92;

pub fn format_of(path: &Path) -> Result<ImageFormat> {
    ImageFormat::from_path(path).map_err(|_| CoreError::Unsupported(path.to_owned()))
}

pub fn encode(img: &DynamicImage, format: ImageFormat, path: &Path) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    let res = match format {
        ImageFormat::Jpeg => {
            // JPEG has no alpha, so convert to RGB first.
            JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&img.to_rgb8())
        }
        ImageFormat::Gif => return Err(CoreError::Unsupported(path.to_owned())),
        other => img.write_to(&mut out, other),
    };
    res.map_err(|e| CoreError::image(path, e))?;
    Ok(out.into_inner())
}

/// Attaches the original's EXIF and ICC profile to the newly encoded bytes.
/// Orientation is set to 1 afterwards because the pixels are already rotated.
pub fn carry_metadata(original: &[u8], encoded: Vec<u8>) -> Vec<u8> {
    let Ok(Some(old)) = DynImage::from_bytes(Bytes::copy_from_slice(original)) else {
        return encoded;
    };
    let Ok(Some(mut new)) = DynImage::from_bytes(Bytes::from(encoded.clone())) else {
        return encoded;
    };
    if let Some(exif_data) = old.exif() {
        let mut data = exif_data.to_vec();
        exif::write_orientation(&mut data, 1);
        new.set_exif(Some(Bytes::from(data)));
    }
    if let Some(icc) = old.icc_profile() {
        new.set_icc_profile(Some(icc));
    }
    new.encoder().bytes().to_vec()
}

/// Writes through a temporary file in the same folder, then renames.
/// If the operation aborts, the original stays untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| CoreError::io(path, e))?;
    tmp.write_all(bytes).map_err(|e| CoreError::io(path, e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| CoreError::io(path, e))?;
    // tempfile creates files with 0600. Existing files keep their permissions,
    // new ones get the usual 0644.
    let perms = match std::fs::metadata(path) {
        Ok(meta) => meta.permissions(),
        Err(_) => std::os::unix::fs::PermissionsExt::from_mode(0o644),
    };
    std::fs::set_permissions(tmp.path(), perms).map_err(|e| CoreError::io(path, e))?;
    tmp.persist(path)
        .map_err(|e| CoreError::io(path, e.error))?;
    Ok(())
}
