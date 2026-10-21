//! XMP-Sidecar neben einer Datei (`foto.CR3` → `foto.CR3.xmp`, wie darktable).
//! Blick legt dort Drehungen von Dateien ab, die es nicht selbst schreibt
//! (RAW, HEIC, AVIF, JPEG XL), und gemerkte Korrekturen. Bei RAW liest Blick
//! zusätzlich `foto.xmp` von Lightroom.

use std::path::{Path, PathBuf};

use crate::encode::write_atomic;
use crate::error::{CoreError, Result};

/// `foto.CR3` → `foto.CR3.xmp`. Eindeutig pro Datei, auch wenn eine Kamera
/// `foto.CR3` und `foto.JPG` nebeneinander ablegt.
pub fn path_for(image: &Path) -> PathBuf {
    let mut p = image.as_os_str().to_owned();
    p.push(".xmp");
    PathBuf::from(p)
}

/// Wo eine Drehung stehen kann: Blicks eigenes Sidecar, bei RAW zusätzlich
/// `foto.xmp` von Lightroom.
fn candidates(image: &Path) -> Vec<PathBuf> {
    let mut v = vec![path_for(image)];
    if crate::formats::kind_of(image) == Some(crate::formats::Kind::Raw) {
        v.push(image.with_extension("xmp"));
    }
    v
}

/// Orientation aus dem ersten vorhandenen Sidecar, als Attribut oder Element.
pub fn read_orientation(image: &Path) -> Option<u16> {
    candidates(image).iter().find_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        parse_orientation(&text)
    })
}

fn parse_orientation(xmp: &str) -> Option<u16> {
    let value = if let Some(i) = xmp.find("tiff:Orientation=\"") {
        let rest = &xmp[i + 18..];
        &rest[..rest.find('"')?]
    } else {
        let i = xmp.find("<tiff:Orientation>")?;
        let rest = &xmp[i + 18..];
        &rest[..rest.find('<')?]
    };
    value.trim().parse().ok().filter(|v| (1..=8).contains(v))
}

const TEMPLATE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Blick">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
    tiff:Orientation="1"/>
 </rdf:RDF>
</x:xmpmeta>
"#;

/// Setzt `tiff:Orientation` im Sidecar und lässt alles andere darin stehen.
pub fn write_orientation(image: &Path, value: u16) -> Result<()> {
    let path = path_for(image);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_owned(),
        Err(e) => return Err(CoreError::io(&path, e)),
    };
    let updated = set_orientation(&text, value).ok_or_else(|| CoreError::Metadata {
        path: path.clone(),
        reason: "kein rdf:Description, in das die Drehung passt".into(),
    })?;
    write_atomic(&path, updated.as_bytes())
}

fn set_orientation(xmp: &str, value: u16) -> Option<String> {
    set_attr(
        xmp,
        "tiff:Orientation",
        &value.to_string(),
        ("tiff", "http://ns.adobe.com/tiff/1.0/"),
    )
}

fn escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

fn unescape(v: &str) -> String {
    v.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// Setzt ein Attribut (oder den Text eines gleichnamigen Elements) im ersten
/// `rdf:Description`; fehlt es, kommt es samt Namespace dazu.
fn set_attr(xmp: &str, qname: &str, value: &str, ns: (&str, &str)) -> Option<String> {
    let value = escape(value);
    let replace_between =
        |start: usize, end: usize| format!("{}{value}{}", &xmp[..start], &xmp[end..]);
    let attr = format!("{qname}=\"");
    if let Some(i) = xmp.find(&attr) {
        let start = i + attr.len();
        let end = start + xmp[start..].find('"')?;
        return Some(replace_between(start, end));
    }
    let open = format!("<{qname}>");
    if let Some(i) = xmp.find(&open) {
        let start = i + open.len();
        let end = start + xmp[start..].find('<')?;
        return Some(replace_between(start, end));
    }
    let i = xmp.find("<rdf:Description")? + "<rdf:Description".len();
    let decl = format!("xmlns:{}=", ns.0);
    let ns = if xmp.contains(&decl) {
        String::new()
    } else {
        format!(" xmlns:{}=\"{}\"", ns.0, ns.1)
    };
    Some(format!(
        "{}{ns} {qname}=\"{value}\"{}",
        &xmp[..i],
        &xmp[i..]
    ))
}

fn get_attr(xmp: &str, qname: &str) -> Option<String> {
    let attr = format!("{qname}=\"");
    if let Some(i) = xmp.find(&attr) {
        let rest = &xmp[i + attr.len()..];
        return Some(unescape(&rest[..rest.find('"')?]));
    }
    let open = format!("<{qname}>");
    let i = xmp.find(&open)?;
    let rest = &xmp[i + open.len()..];
    Some(unescape(&rest[..rest.find('<')?]))
}

// ---------- Bearbeitung ohne die Datei anzufassen ----------

const BLICK_NS: (&str, &str) = ("blick", "https://github.com/paulhoever/Blick/ns/1.0/");

/// Regler und Stil, gespeichert neben dem Bild. Blick wendet sie beim Öffnen an;
/// andere Programme ignorieren den Namespace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditSettings {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    /// Pfad des Stils relativ zum Stilordner, oder Name eines mitgelieferten Stils.
    pub look: Option<String>,
    pub look_strength: f32,
    /// Zuschnitt im gedrehten Bild, `None` = ganzes Bild.
    pub crop: Option<crate::crop::Crop>,
}

impl EditSettings {
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 0.0
            && self.saturation == 0.0
            && self.look.is_none()
            && self.crop.is_none()
    }
}

fn parse_crop(v: &str) -> Option<crate::crop::Crop> {
    let n: Vec<f64> = v
        .split_whitespace()
        .map(|s| s.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, w, h] = n[..] else { return None };
    crate::crop::Crop { x, y, w, h }
        .sanitized()
        .filter(|c| !c.is_identity())
}

fn format_crop(c: Option<crate::crop::Crop>) -> String {
    c.map(|c| format!("{:.5} {:.5} {:.5} {:.5}", c.x, c.y, c.w, c.h))
        .unwrap_or_default()
}

/// `foto.jpg` → `foto.jpg.xmp`. So kollidiert es nicht mit `foto.xmp` einer
/// RAW-Datei gleichen Namens (RAW+JPEG aus der Kamera).
pub fn edit_path(image: &Path) -> PathBuf {
    path_for(image)
}

pub fn read_edit(image: &Path) -> Option<EditSettings> {
    let text = std::fs::read_to_string(edit_path(image)).ok()?;
    let num = |k: &str| get_attr(&text, k).and_then(|v| v.parse::<f32>().ok());
    let crop = get_attr(&text, "blick:Crop");
    if num("blick:Brightness").is_none() && crop.is_none() {
        return None;
    }
    Some(EditSettings {
        brightness: num("blick:Brightness").unwrap_or(0.0),
        contrast: num("blick:Contrast").unwrap_or(0.0),
        saturation: num("blick:Saturation").unwrap_or(0.0),
        look: get_attr(&text, "blick:Look").filter(|s| !s.is_empty()),
        look_strength: num("blick:LookStrength").unwrap_or(1.0),
        crop: crop.as_deref().and_then(parse_crop),
    })
}

pub fn write_edit(image: &Path, e: &EditSettings) -> Result<()> {
    let path = edit_path(image);
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            TEMPLATE.replace("\n    tiff:Orientation=\"1\"", "")
        }
        Err(err) => return Err(CoreError::io(&path, err)),
    };
    let fields = [
        ("blick:Brightness", format!("{:.3}", e.brightness)),
        ("blick:Contrast", format!("{:.3}", e.contrast)),
        ("blick:Saturation", format!("{:.3}", e.saturation)),
        ("blick:Look", e.look.clone().unwrap_or_default()),
        ("blick:LookStrength", format!("{:.3}", e.look_strength)),
        ("blick:Crop", format_crop(e.crop)),
    ];
    for (k, v) in fields {
        text = set_attr(&text, k, &v, BLICK_NS).ok_or_else(|| CoreError::Metadata {
            path: path.clone(),
            reason: "kein rdf:Description im Sidecar".into(),
        })?;
    }
    write_atomic(&path, text.as_bytes())
}

/// Nach einer Drehung den gemerkten Zuschnitt mitdrehen. Ohne Zuschnitt
/// bleibt das Sidecar unberührt.
pub fn rotate_crop(image: &Path, rot: crate::rotate::Rotation) -> Result<()> {
    match read_edit(image) {
        Some(mut e) if e.crop.is_some() => {
            e.crop = e.crop.map(|c| c.rotated(rot));
            write_edit(image, &e)
        }
        _ => Ok(()),
    }
}

/// Gemerkte Bearbeitung vergessen, etwa nachdem sie ins Original geschrieben
/// wurde. Andere Felder im Sidecar (Drehung, fremde Programme) bleiben.
pub fn clear_edit(image: &Path) -> Result<()> {
    match read_edit(image) {
        Some(e) if !e.is_identity() => write_edit(
            image,
            &EditSettings {
                look_strength: 1.0,
                ..Default::default()
            },
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_reads_and_updates() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("foto.CR3");
        std::fs::write(&raw, b"raw").unwrap();
        assert_eq!(read_orientation(&raw), None);
        write_orientation(&raw, 6).unwrap();
        assert_eq!(read_orientation(&raw), Some(6));
        write_orientation(&raw, 3).unwrap();
        assert_eq!(read_orientation(&raw), Some(3));
        assert_eq!(std::fs::read(&raw).unwrap(), b"raw");
    }

    #[test]
    fn edit_roundtrip_keeps_other_fields() {
        let d = tempfile::tempdir().unwrap();
        let img = d.path().join("a.jpg");
        std::fs::write(&img, b"jpg").unwrap();
        assert_eq!(read_edit(&img), None);
        let e = EditSettings {
            brightness: 0.2,
            contrast: -0.1,
            saturation: 0.0,
            look: Some("RawTherapee/Color/Kodak/Kodak Portra 400 2.png".into()),
            look_strength: 0.75,
            crop: None,
        };
        write_edit(&img, &e).unwrap();
        assert_eq!(read_edit(&img), Some(e.clone()));
        let e2 = EditSettings {
            look: Some("Name mit \"Anführung\" & Co".into()),
            ..e
        };
        write_edit(&img, &e2).unwrap();
        assert_eq!(read_edit(&img), Some(e2));
        assert_eq!(std::fs::read(&img).unwrap(), b"jpg");
        assert!(edit_path(&img).ends_with("a.jpg.xmp"));
    }

    #[test]
    fn crop_roundtrip_rotate_and_clear() {
        use crate::crop::Crop;
        use crate::rotate::Rotation;
        let d = tempfile::tempdir().unwrap();
        let img = d.path().join("b.png");
        std::fs::write(&img, b"png").unwrap();
        // Ohne Sidecar: Drehen und Vergessen legen nichts an.
        rotate_crop(&img, Rotation::Right).unwrap();
        clear_edit(&img).unwrap();
        assert!(!edit_path(&img).exists());

        let crop = Crop {
            x: 0.1,
            y: 0.2,
            w: 0.5,
            h: 0.25,
        };
        // Nur ein Zuschnitt, keine Regler: wird trotzdem gelesen.
        write_orientation(&img, 1).unwrap();
        let only_crop = EditSettings {
            look_strength: 1.0,
            crop: Some(crop),
            ..Default::default()
        };
        write_edit(&img, &only_crop).unwrap();
        assert_eq!(read_edit(&img), Some(only_crop.clone()));

        rotate_crop(&img, Rotation::Right).unwrap();
        let turned = read_edit(&img).unwrap().crop.unwrap();
        let want = crop.rotated(Rotation::Right);
        assert!((turned.x - want.x).abs() < 1e-4 && (turned.h - want.h).abs() < 1e-4);
        assert_eq!(read_orientation(&img), Some(1), "Drehung im Sidecar bleibt");

        clear_edit(&img).unwrap();
        let cleared = read_edit(&img).unwrap();
        assert!(cleared.is_identity());
        assert_eq!(read_orientation(&img), Some(1));
    }

    #[test]
    fn broken_or_full_crop_is_none() {
        assert_eq!(parse_crop(""), None);
        assert_eq!(parse_crop("0 0 1"), None);
        assert_eq!(parse_crop("0 0 1 1"), None);
        assert_eq!(parse_crop("a b c d"), None);
        assert!(parse_crop("0.5 0 0.5 1").is_some());
    }

    #[test]
    fn keeps_foreign_content() {
        let lr = r#"<x:xmpmeta><rdf:RDF><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50"/></rdf:RDF></x:xmpmeta>"#;
        let out = set_orientation(lr, 8).unwrap();
        assert!(out.contains("crs:Exposure2012=\"+0.50\""));
        assert_eq!(parse_orientation(&out), Some(8));
    }

    #[test]
    fn same_stem_does_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let (raw, jpg) = (d.path().join("foto.CR3"), d.path().join("foto.JPG"));
        write_orientation(&raw, 6).unwrap();
        write_orientation(&jpg, 8).unwrap();
        assert_eq!(read_orientation(&raw), Some(6));
        assert_eq!(read_orientation(&jpg), Some(8));
        // Lightroom-Sidecar ohne Endung gilt nur für RAW.
        std::fs::write(
            d.path().join("bild.xmp"),
            "<tiff:Orientation>3</tiff:Orientation>",
        )
        .unwrap();
        assert_eq!(read_orientation(&d.path().join("bild.NEF")), Some(3));
        assert_eq!(read_orientation(&d.path().join("bild.heic")), None);
    }

    #[test]
    fn reads_element_form_and_darktable_name() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("a.NEF");
        std::fs::write(
            d.path().join("a.NEF.xmp"),
            "<tiff:Orientation>8</tiff:Orientation>",
        )
        .unwrap();
        assert_eq!(read_orientation(&raw), Some(8));
    }
}
