//! Welche Dateien Blick kennt und wer sie dekodiert.

use std::path::Path;

/// Wer ein Format dekodiert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `image`-Crate im Prozess: schnell, auch auf dem Host ohne Laufzeit.
    Raster,
    /// Kamera-RAW über `rawler`.
    Raw,
    /// Alles, was nur der registrierte Rückfall-Dekodierer kann (im Flatpak glycin).
    Other,
    /// Video: abspielen in der App (GStreamer), Standbild über den registrierten Bildgreifer.
    Video,
}

const RASTER: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "webp", "gif", "bmp", "tif", "tiff",
];

/// Formate, die glycin in der GNOME-Laufzeit lädt und `image` hier nicht.
const OTHER: &[&str] = &[
    "heic", "heif", "avif", "jxl", "svg", "svgz", "exr", "hdr", "qoi", "tga", "ico", "dds", "pnm",
    "pbm", "pgm", "ppm", "xpm", "xbm",
];

/// Container, die GStreamer in der GNOME-Laufzeit abspielt.
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "3gp", "mts", "m2ts", "ogv", "mpg", "mpeg", "wmv",
];

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = ext_lower(path)?;
    if RASTER.contains(&ext.as_str()) {
        Some(Kind::Raster)
    } else if rawler::decoders::supported_extensions()
        .iter()
        .any(|r| r.eq_ignore_ascii_case(&ext))
    {
        Some(Kind::Raw)
    } else if OTHER.contains(&ext.as_str()) {
        Some(Kind::Other)
    } else if VIDEO.contains(&ext.as_str()) {
        Some(Kind::Video)
    } else {
        None
    }
}

pub fn is_video(path: &Path) -> bool {
    kind_of(path) == Some(Kind::Video)
}

/// Alle bekannten Endungen, klein geschrieben. Für Dateidialog und Desktop-Datei.
pub fn extensions() -> Vec<String> {
    let mut v: Vec<String> = RASTER
        .iter()
        .chain(OTHER)
        .chain(VIDEO)
        .map(|s| s.to_string())
        .collect();
    v.extend(
        rawler::decoders::supported_extensions()
            .iter()
            .map(|s| s.to_ascii_lowercase()),
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(Path::new("a.JPG")), Some(Kind::Raster));
        assert_eq!(kind_of(Path::new("a.CR3")), Some(Kind::Raw));
        assert_eq!(kind_of(Path::new("a.nef")), Some(Kind::Raw));
        assert_eq!(kind_of(Path::new("a.dng")), Some(Kind::Raw));
        assert_eq!(kind_of(Path::new("a.heic")), Some(Kind::Other));
        assert_eq!(kind_of(Path::new("a.jxl")), Some(Kind::Other));
        assert_eq!(kind_of(Path::new("a.MOV")), Some(Kind::Video));
        assert_eq!(kind_of(Path::new("a.mp4")), Some(Kind::Video));
        assert!(is_video(Path::new("clip.webm")));
        assert!(!is_video(Path::new("a.jpg")));
        assert_eq!(kind_of(Path::new("a.txt")), None);
        assert_eq!(kind_of(Path::new("README")), None);
    }
}
