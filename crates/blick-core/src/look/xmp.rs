//! Lightroom- und Camera-Raw-Presets (`.xmp` mit `crs:`-Feldern).
//!
//! Adobe veröffentlicht für diese Felder keine Spezifikation. Namen und
//! Wertebereiche folgen dem, was Lightroom beim Export schreibt und ExifTool
//! dokumentiert. Felder, die Blick nicht umsetzen kann, landen in
//! `Preset::ignored`, damit der Import sie nennen kann.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use super::effects::{Grain, Vignette};
use super::params::{Curve, HSL_NAMES, Params, Tint};

#[derive(Debug, Clone, Default)]
pub struct Preset {
    pub name: Option<String>,
    pub params: Params,
    pub grain: Grain,
    pub vignette: Vignette,
    /// Gesetzte Felder, die Blick nicht abbildet (etwa Profile, Masken, Objektivkorrektur).
    pub ignored: Vec<String>,
}

/// Felder ohne Bildwirkung oder mit Bedeutung nur für Lightroom selbst.
const META: &[&str] = &[
    "Version",
    "ProcessVersion",
    "PresetType",
    "UUID",
    "SupportsAmount",
    "SupportsAmount2",
    "SupportsColor",
    "SupportsMonochrome",
    "SupportsHighDynamicRange",
    "SupportsNormalDynamicRange",
    "SupportsSceneReferred",
    "SupportsOutputReferred",
    "Cluster",
    "Group",
    "Name",
    "SortName",
    "ShortName",
    "Copyright",
    "ContactInfo",
    "Description",
    "HasSettings",
    "HasCrop",
    "AlreadyApplied",
    "CameraModelRestriction",
    "RequiresRGBTables",
    "ToneCurveName2012",
    "ToneCurveName",
    "WhiteBalance",
    "CameraProfile",
    "CameraProfileDigest",
    "Look",
    "OverrideLookVignette",
    "CompatibleVersion",
    "GrainSeed",
    "AutoLateralCA",
    "LensProfileEnable",
];

/// Liest alle `crs:`-Attribute und -Elemente in eine flache Tabelle.
/// Kurven (`rdf:Seq` aus `rdf:li`) werden mit `;` verbunden.
fn collect(xml: &str) -> Result<BTreeMap<String, String>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut items: Vec<String> = Vec::new();
    loop {
        match reader.read_event().map_err(|e| format!("XML: {e}"))? {
            Event::Start(e) | Event::Empty(e) => {
                for a in e.attributes().flatten() {
                    let key: &str = a.key.as_ref();
                    if let Some(k) = key.strip_prefix("crs:") {
                        let v = a
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map(|v| v.into_owned())
                            .unwrap_or_default();
                        out.insert(k.to_owned(), v);
                    }
                }
                let name = e.name();
                let tag: &str = name.as_ref();
                if let Some(k) = tag.strip_prefix("crs:") {
                    current = Some(k.to_owned());
                    items.clear();
                }
            }
            Event::Text(t) => {
                if current.is_some() {
                    let text = t.xml10_content();
                    if !text.trim().is_empty() {
                        items.push(text.trim().to_owned());
                    }
                }
            }
            Event::End(e) => {
                let name = e.name();
                let tag: &str = name.as_ref();
                if let Some(k) = tag.strip_prefix("crs:")
                    && current.as_deref() == Some(k)
                {
                    out.insert(k.to_owned(), items.join(";"));
                    current = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn num(map: &BTreeMap<String, String>, key: &str) -> Option<f32> {
    map.get(key)
        .and_then(|v| v.trim().trim_start_matches('+').parse().ok())
}

fn curve(map: &BTreeMap<String, String>, key: &str) -> Curve {
    let Some(v) = map.get(key) else {
        return Curve::default();
    };
    let points = v
        .split(';')
        .filter_map(|p| {
            let (x, y) = p.split_once(',')?;
            Some((
                x.trim().parse::<f32>().ok()? / 255.0,
                y.trim().parse::<f32>().ok()? / 255.0,
            ))
        })
        .collect();
    Curve { points }
}

pub fn parse(xml: &str) -> Result<Preset, String> {
    let map = collect(xml)?;
    if map.is_empty() {
        return Err("keine Camera-Raw-Einstellungen (crs:) gefunden".into());
    }
    let mut used: Vec<&str> = Vec::new();
    let mut get = |k: &'static str| {
        used.push(k);
        num(&map, k)
    };

    let mut p = Params {
        exposure: get("Exposure2012").or(get("Exposure")).unwrap_or(0.0),
        contrast: get("Contrast2012").unwrap_or(0.0),
        highlights: get("Highlights2012").unwrap_or(0.0),
        shadows: get("Shadows2012").unwrap_or(0.0),
        whites: get("Whites2012").unwrap_or(0.0),
        blacks: get("Blacks2012").unwrap_or(0.0),
        vibrance: get("Vibrance").unwrap_or(0.0),
        saturation: get("Saturation").unwrap_or(0.0),
        temperature: get("IncrementalTemperature").unwrap_or(0.0),
        tint: get("IncrementalTint").unwrap_or(0.0),
        ..Default::default()
    };
    for (k, name) in HSL_NAMES.iter().enumerate() {
        for (j, kind) in ["Hue", "Saturation", "Luminance"].iter().enumerate() {
            let key = format!("{kind}Adjustment{name}");
            if let Some(v) = num(&map, &key) {
                p.hsl[k][j] = v;
            }
        }
    }
    p.grayscale = map
        .get("ConvertToGrayscale")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    p.curve = curve(&map, "ToneCurvePV2012");
    p.curve_rgb = [
        curve(&map, "ToneCurvePV2012Red"),
        curve(&map, "ToneCurvePV2012Green"),
        curve(&map, "ToneCurvePV2012Blue"),
    ];

    // Split-Toning (alt) oder Color Grading (neu, ab Lightroom 10).
    let sh = (
        num(&map, "SplitToningShadowHue"),
        num(&map, "SplitToningShadowSaturation"),
    );
    let hi = (
        num(&map, "SplitToningHighlightHue"),
        num(&map, "SplitToningHighlightSaturation"),
    );
    let cg_sh = (
        num(&map, "ColorGradeShadowHue"),
        num(&map, "ColorGradeShadowSat"),
    );
    let cg_hi = (
        num(&map, "ColorGradeHighlightHue"),
        num(&map, "ColorGradeHighlightSat"),
    );
    let pick = |a: (Option<f32>, Option<f32>), b: (Option<f32>, Option<f32>)| {
        let (h, s) = if a.1.unwrap_or(0.0) > 0.0 { a } else { b };
        Tint {
            hue: h.unwrap_or(0.0),
            saturation: s.unwrap_or(0.0),
        }
    };
    p.split_shadows = pick(sh, cg_sh);
    p.split_highlights = pick(hi, cg_hi);
    p.split_balance = num(&map, "SplitToningBalance").unwrap_or(0.0);

    let grain = Grain {
        amount: num(&map, "GrainAmount").unwrap_or(0.0),
        size: num(&map, "GrainSize").unwrap_or(25.0),
        roughness: num(&map, "GrainFrequency").unwrap_or(50.0),
    };
    let vignette = Vignette {
        amount: num(&map, "PostCropVignetteAmount").unwrap_or(0.0),
        midpoint: num(&map, "PostCropVignetteMidpoint").unwrap_or(50.0),
    };

    let known = |k: &str| {
        META.contains(&k)
            || k.starts_with("HueAdjustment")
            || k.starts_with("SaturationAdjustment")
            || k.starts_with("LuminanceAdjustment")
            || k.starts_with("ToneCurvePV2012")
            || k.starts_with("SplitToning")
            || k.starts_with("Grain")
            || k.starts_with("PostCropVignette")
            || matches!(
                k,
                "Exposure2012"
                    | "Exposure"
                    | "Contrast2012"
                    | "Highlights2012"
                    | "Shadows2012"
                    | "Whites2012"
                    | "Blacks2012"
                    | "Vibrance"
                    | "Saturation"
                    | "IncrementalTemperature"
                    | "IncrementalTint"
                    | "ConvertToGrayscale"
                    | "ColorGradeShadowHue"
                    | "ColorGradeShadowSat"
                    | "ColorGradeHighlightHue"
                    | "ColorGradeHighlightSat"
            )
    };
    // Nur Felder melden, die tatsächlich etwas tun würden.
    let neutral = |v: &str| {
        let v = v.trim().trim_start_matches('+');
        v.is_empty()
            || v == "0"
            || v == "0.00"
            || v.eq_ignore_ascii_case("false")
            || v == "0, 0;255, 255"
    };
    let ignored = map
        .iter()
        .filter(|(k, v)| !known(k) && !neutral(v))
        .map(|(k, _)| k.clone())
        .collect();

    let name = map
        .get("Name")
        .map(|n| n.split(';').next().unwrap_or(n).to_owned())
        .filter(|n| !n.is_empty());
    Ok(Preset {
        name,
        params: p,
        grain,
        vignette,
        ignored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:PresetType="Normal" crs:Version="15.0" crs:Exposure2012="+0.35" crs:Contrast2012="+20"
   crs:Shadows2012="+15" crs:Vibrance="-10" crs:SaturationAdjustmentBlue="-40"
   crs:SplitToningShadowHue="210" crs:SplitToningShadowSaturation="20"
   crs:GrainAmount="25" crs:GrainSize="30" crs:PostCropVignetteAmount="-15"
   crs:LensProfileEnable="1" crs:EnableMaskGroupBasedCorrections="True" crs:Texture="+12">
   <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Portra-ähnlich</rdf:li></rdf:Alt></crs:Name>
   <crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 18</rdf:li><rdf:li>128, 130</rdf:li><rdf:li>255, 245</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn reads_lightroom_preset() {
        let p = parse(LR).unwrap();
        assert_eq!(p.name.as_deref(), Some("Portra-ähnlich"));
        assert!((p.params.exposure - 0.35).abs() < 1e-6);
        assert_eq!(p.params.contrast, 20.0);
        assert_eq!(p.params.hsl[5][1], -40.0);
        assert_eq!(
            p.params.split_shadows,
            Tint {
                hue: 210.0,
                saturation: 20.0
            }
        );
        assert_eq!(p.params.curve.points.len(), 3);
        assert!((p.params.curve.points[0].1 - 18.0 / 255.0).abs() < 1e-6);
        assert_eq!(p.grain.amount, 25.0);
        assert_eq!(p.vignette.amount, -15.0);
        assert_eq!(p.ignored, ["EnableMaskGroupBasedCorrections", "Texture"]);
    }

    #[test]
    fn rejects_non_preset() {
        assert!(parse("<x:xmpmeta><rdf:RDF/></x:xmpmeta>").is_err());
        assert!(parse("kein xml <<<").is_err());
    }
}
