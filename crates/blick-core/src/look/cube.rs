//! `.cube` files (Adobe/Resolve format) for 3D and 1D LUTs.

use super::lut::Lut3d;

#[derive(Debug, Clone)]
pub struct Cube {
    pub title: Option<String>,
    pub lut: Lut3d,
}

pub fn parse(text: &str) -> Result<Cube, String> {
    let mut title = None;
    let mut size3 = None;
    let mut size1 = None;
    let mut min = [0.0f32; 3];
    let mut max = [1.0f32; 3];
    let mut rows: Vec<[f32; 3]> = Vec::new();

    let triple = |parts: &[&str]| -> Result<[f32; 3], String> {
        if parts.len() < 3 {
            return Err(format!("drei Zahlen erwartet: {}", parts.join(" ")));
        }
        let p = |s: &str| s.parse::<f32>().map_err(|_| format!("not a number: {s}"));
        Ok([p(parts[0])?, p(parts[1])?, p(parts[2])?])
    };

    for (no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let err = |e: String| format!("Zeile {}: {e}", no + 1);
        match parts[0] {
            "TITLE" => title = Some(line[5..].trim().trim_matches('"').to_owned()),
            "LUT_3D_SIZE" => {
                size3 = Some(
                    parts
                        .get(1)
                        .and_then(|s| s.parse::<usize>().ok())
                        .ok_or_else(|| err("size missing".into()))?,
                )
            }
            "LUT_1D_SIZE" => {
                size1 = Some(
                    parts
                        .get(1)
                        .and_then(|s| s.parse::<usize>().ok())
                        .ok_or_else(|| err("size missing".into()))?,
                )
            }
            "DOMAIN_MIN" => min = triple(&parts[1..]).map_err(err)?,
            "DOMAIN_MAX" => max = triple(&parts[1..]).map_err(err)?,
            // Other keywords (LUT_3D_INPUT_RANGE and so on) don't matter here.
            k if k.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {}
            _ => rows.push(triple(&parts).map_err(err)?),
        }
    }

    let norm = |c: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|i| ((c[i] - min[i]) / (max[i] - min[i]).max(1e-6)).clamp(0.0, 1.0))
    };

    let lut = match (size3, size1) {
        (Some(n), _) => {
            if !(2..=256).contains(&n) || rows.len() != n * n * n {
                return Err(format!(
                    "LUT_3D_SIZE {n} erwartet {} Zeilen, gefunden {}",
                    n * n * n,
                    rows.len()
                ));
            }
            Lut3d {
                size: n,
                data: rows.into_iter().map(norm).collect(),
            }
        }
        (None, Some(n)) => {
            if n < 2 || rows.len() != n {
                return Err(format!(
                    "LUT_1D_SIZE {n} erwartet {n} Zeilen, gefunden {}",
                    rows.len()
                ));
            }
            let curves: Vec<[f32; 3]> = rows.into_iter().map(norm).collect();
            let curve = |ch: usize, x: f32| {
                let p = x.clamp(0.0, 1.0) * (n - 1) as f32;
                let i = (p.floor() as usize).min(n - 2);
                let t = p - i as f32;
                curves[i][ch] + (curves[i + 1][ch] - curves[i][ch]) * t
            };
            Lut3d::from_fn(33, |c| [curve(0, c[0]), curve(1, c[1]), curve(2, c[2])])
        }
        (None, None) => return Err("neither LUT_3D_SIZE nor LUT_1D_SIZE".into()),
    };
    Ok(Cube { title, lut })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_text() {
        let lut = Lut3d::from_fn(5, |c| [c[1], c[2], c[0]]);
        let back = parse(&lut.to_cube("Tausch")).unwrap();
        assert_eq!(back.title.as_deref(), Some("Tausch"));
        assert_eq!(back.lut.size, 5);
        let o = back.lut.sample([0.2, 0.5, 0.9]);
        assert!(
            (o[0] - 0.5).abs() < 1e-4 && (o[1] - 0.9).abs() < 1e-4 && (o[2] - 0.2).abs() < 1e-4
        );
    }

    #[test]
    fn domain_and_1d() {
        let text = "# Kommentar\nLUT_1D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n1 1 1\n0 0 0\n";
        let c = parse(text).unwrap();
        let o = c.lut.sample([0.25, 0.25, 0.25]);
        assert!((o[0] - 0.75).abs() < 1e-3);
    }

    #[test]
    fn rejects_wrong_count() {
        assert!(parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(parse("1 2 3\n").is_err());
    }
}
