//! Which files Blick knows and who decodes them.

use std::path::Path;

/// Who decodes a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The `image` crate in-process: fast, also on the host without a runtime.
    Raster,
    /// Camera RAW through `rawler`.
    Raw,
    /// Everything only the registered fallback decoder can do (glycin in the Flatpak).
    Other,
    /// Video: played in the app (GStreamer), still frame through the registered frame grabber.
    Video,
}

const RASTER: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "webp", "gif", "bmp", "tif", "tiff",
];

/// Formats glycin loads in the GNOME runtime and `image` doesn't here.
const OTHER: &[&str] = &[
    "heic", "heif", "avif", "jxl", "svg", "svgz", "exr", "hdr", "qoi", "tga", "ico", "dds", "pnm",
    "pbm", "pgm", "ppm", "xpm", "xbm",
];

/// Containers GStreamer plays in the GNOME runtime.
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

/// All known extensions, lowercase. For the file dialog and the desktop file.
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
