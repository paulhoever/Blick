//! Image information for the info bar: file, capture, camera.

use std::path::Path;
use std::time::SystemTime;

use crate::formats::{Kind, kind_of};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Info {
    pub file_size: u64,
    pub dimensions: Option<(u32, u32)>,
    pub modified: Option<SystemTime>,
    /// As in EXIF, `YYYY:MM:DD hh:mm:ss`.
    pub taken: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<String>,
    pub iso: Option<String>,
    pub focal_length: Option<String>,
}

impl Info {
    /// Rows for display, empty fields are dropped.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut v = vec![("File size", human_size(self.file_size))];
        if let Some((w, h)) = self.dimensions {
            v.push((
                "Dimensions",
                format!("{w} × {h} ({:.1} MP)", w as f64 * h as f64 / 1e6),
            ));
        }
        let opt = [
            ("Taken", self.taken.as_ref().map(|t| pretty_date(t))),
            ("Camera", self.camera.clone()),
            ("Lens", self.lens.clone()),
            ("Exposure", self.exposure.clone()),
            ("Aperture", self.aperture.clone()),
            ("ISO", self.iso.clone()),
            ("Focal length", self.focal_length.clone()),
        ];
        v.extend(opt.into_iter().filter_map(|(k, val)| Some((k, val?))));
        v
    }
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

/// `2026:10:09 14:03:21` → `2026-10-09 14:03`
fn pretty_date(s: &str) -> String {
    let b = s.as_bytes();
    if s.len() >= 16 && b[4] == b':' && b[7] == b':' {
        format!("{}-{}-{} {}", &s[0..4], &s[5..7], &s[8..10], &s[11..16])
    } else {
        s.to_owned()
    }
}

fn camera(make: Option<String>, model: Option<String>) -> Option<String> {
    match (make, model) {
        // Many cameras repeat the maker in the model name.
        (Some(make), Some(model))
            if model.to_lowercase().starts_with(
                &make
                    .to_lowercase()
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
            ) =>
        {
            Some(model)
        }
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (a, b) => a.or(b),
    }
}

pub fn read(path: &Path) -> Info {
    let meta = std::fs::metadata(path).ok();
    let mut info = Info {
        file_size: meta.as_ref().map_or(0, |m| m.len()),
        modified: meta.and_then(|m| m.modified().ok()),
        ..Default::default()
    };
    match kind_of(path) {
        Some(Kind::Raw) => read_raw(path, &mut info),
        // The player supplies duration and dimensions, there's no EXIF here.
        Some(Kind::Video) => {}
        _ => {
            info.dimensions = image::ImageReader::open(path)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.into_dimensions().ok());
            read_exif(path, &mut info)
        }
    }
    info
}

fn read_exif(path: &Path, info: &mut Info) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let Ok(ex) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)) else {
        return;
    };
    let get = |tag| {
        ex.get_field(tag, exif::In::PRIMARY)
            .map(|f| {
                f.display_value()
                    .with_unit(&ex)
                    .to_string()
                    .trim_matches('"')
                    .trim()
                    .to_owned()
            })
            .filter(|s| !s.is_empty())
    };
    let ascii = |tag| {
        ex.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| match &f.value {
                exif::Value::Ascii(v) => v
                    .first()
                    .map(|s| String::from_utf8_lossy(s).trim().to_owned()),
                _ => None,
            })
    };
    info.taken = ascii(exif::Tag::DateTimeOriginal).or_else(|| ascii(exif::Tag::DateTime));
    info.camera = camera(ascii(exif::Tag::Make), ascii(exif::Tag::Model));
    info.lens = ascii(exif::Tag::LensModel);
    info.exposure = get(exif::Tag::ExposureTime);
    info.aperture = get(exif::Tag::FNumber);
    info.iso = get(exif::Tag::PhotographicSensitivity);
    info.focal_length = get(exif::Tag::FocalLength);
}

fn read_raw(path: &Path, info: &mut Info) {
    use rawler::decoders::RawDecodeParams;
    let Ok(src) = rawler::rawsource::RawSource::new(path) else {
        return;
    };
    let Ok(dec) = rawler::get_decoder(&src) else {
        return;
    };
    let Ok(md) = dec.raw_metadata(&src, &RawDecodeParams::default()) else {
        return;
    };
    let e = md.exif;
    let ratio = |r: rawler::formats::tiff::Rational| {
        if r.d == 0 {
            0.0
        } else {
            r.n as f64 / r.d as f64
        }
    };
    info.taken = e.date_time_original.or(e.create_date);
    info.camera = camera(
        Some(md.make).filter(|s| !s.is_empty()),
        Some(md.model).filter(|s| !s.is_empty()),
    );
    info.lens = e.lens_model.or(md.lens.map(|l| l.lens_model));
    info.exposure = e.exposure_time.map(|r| {
        let t = ratio(r);
        if t > 0.0 && t < 1.0 {
            format!("1/{:.0} s", 1.0 / t)
        } else {
            format!("{t} s")
        }
    });
    info.aperture = e.fnumber.map(|r| format!("f/{:.1}", ratio(r)));
    info.iso = e
        .iso_speed_ratings
        .map(|v| v.to_string())
        .or(e.iso_speed.map(|v| v.to_string()));
    info.focal_length = e.focal_length.map(|r| format!("{:.0} mm", ratio(r)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_values() {
        assert_eq!(pretty_date("2026:10:09 14:03:21"), "2026-10-09 14:03");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(
            camera(Some("NIKON CORPORATION".into()), Some("NIKON Z 6".into())).as_deref(),
            Some("NIKON Z 6")
        );
        assert_eq!(
            camera(Some("Canon".into()), Some("EOS R6".into())).as_deref(),
            Some("Canon EOS R6")
        );
    }

    #[test]
    fn raw_samples_have_camera() {
        let Ok(dir) = std::env::var("BLICK_RAW_SAMPLES") else {
            return;
        };
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if kind_of(&p) == Some(Kind::Raw) {
                let i = read(&p);
                println!("{}: {:?}", p.display(), i.rows());
                assert!(i.camera.is_some(), "{}", p.display());
            }
        }
    }
}
