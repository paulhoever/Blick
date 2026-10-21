//! Kodieren und sicheres Zurückschreiben, Metadaten werden übernommen.

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
            // JPEG kann kein Alpha, also vorher nach RGB.
            JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&img.to_rgb8())
        }
        ImageFormat::Gif => return Err(CoreError::Unsupported(path.to_owned())),
        other => img.write_to(&mut out, other),
    };
    res.map_err(|e| CoreError::image(path, e))?;
    Ok(out.into_inner())
}

/// Hängt EXIF und ICC-Profil des Originals an die neu kodierten Bytes.
/// Die Orientierung steht danach auf 1, weil die Pixel schon gedreht sind.
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

/// Schreibt über eine temporäre Datei im selben Ordner und benennt dann um.
/// Bricht der Vorgang ab, bleibt das Original unangetastet.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| CoreError::io(path, e))?;
    tmp.write_all(bytes).map_err(|e| CoreError::io(path, e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| CoreError::io(path, e))?;
    // tempfile legt mit 0600 an. Bestehende Dateien behalten ihre Rechte,
    // neue bekommen die üblichen 0644.
    let perms = match std::fs::metadata(path) {
        Ok(meta) => meta.permissions(),
        Err(_) => std::os::unix::fs::PermissionsExt::from_mode(0o644),
    };
    std::fs::set_permissions(tmp.path(), perms).map_err(|e| CoreError::io(path, e))?;
    tmp.persist(path)
        .map_err(|e| CoreError::io(path, e.error))?;
    Ok(())
}
