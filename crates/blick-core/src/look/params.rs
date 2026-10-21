//! Globale Farb- und Tonwertparameter nach dem Vorbild von Lightroom/Camera Raw.
//!
//! Lightroom rechnet Lichter, Tiefen, Weiß und Schwarz lokal (mit Rücksicht
//! auf die Umgebung eines Pixels). Hier sind es globale Kurven, damit alles in
//! eine LUT passt. Ein importiertes Preset sieht dadurch ähnlich aus, nicht gleich.

/// Farbbereiche wie in Lightroom: Rot, Orange, Gelb, Grün, Aquamarin, Blau, Lila, Magenta.
pub const HSL_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];
const HSL_CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];

/// Gradationskurve aus Stützpunkten (x, y) in 0..=1, monoton interpoliert.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Curve {
    pub points: Vec<(f32, f32)>,
}

impl Curve {
    pub fn is_identity(&self) -> bool {
        self.points.is_empty() || self.points.iter().all(|(x, y)| (x - y).abs() < 1e-4)
    }

    /// Monotone kubische Interpolation (Fritsch-Carlson), überschwingt nicht.
    pub fn eval(&self, x: f32) -> f32 {
        let p = &self.points;
        if p.len() < 2 {
            return x;
        }
        if x <= p[0].0 {
            return p[0].1;
        }
        if x >= p[p.len() - 1].0 {
            return p[p.len() - 1].1;
        }
        let n = p.len();
        let d: Vec<f32> = (0..n - 1)
            .map(|i| (p[i + 1].1 - p[i].1) / (p[i + 1].0 - p[i].0).max(1e-6))
            .collect();
        let mut m = vec![0.0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 {
                0.0
            } else {
                (d[i - 1] + d[i]) / 2.0
            };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
            } else {
                let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
                let s = a * a + b * b;
                if s > 9.0 {
                    let t = 3.0 / s.sqrt();
                    m[i] = t * a * d[i];
                    m[i + 1] = t * b * d[i];
                }
            }
        }
        let i = (0..n - 1).find(|&i| x < p[i + 1].0).unwrap_or(n - 2);
        let h = p[i + 1].0 - p[i].0;
        let t = (x - p[i].0) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * p[i].1
            + (t3 - 2.0 * t2 + t) * h * m[i]
            + (-2.0 * t3 + 3.0 * t2) * p[i + 1].1
            + (t3 - t2) * h * m[i + 1]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Tint {
    /// Farbton in Grad.
    pub hue: f32,
    /// 0..=100
    pub saturation: f32,
}

/// Werte in den Einheiten von Lightroom: Belichtung in Blenden, fast alles
/// andere -100..=100.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Params {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub vibrance: f32,
    pub saturation: f32,
    /// Weißabgleich relativ, wie bei JPEG-Presets (IncrementalTemperature/Tint).
    pub temperature: f32,
    pub tint: f32,
    pub curve: Curve,
    pub curve_rgb: [Curve; 3],
    /// Je Farbbereich: Farbton, Sättigung, Luminanz.
    pub hsl: [[f32; 3]; 8],
    pub split_shadows: Tint,
    pub split_highlights: Tint,
    /// -100 = mehr Tiefen-Färbung, +100 = mehr Lichter-Färbung.
    pub split_balance: f32,
    pub grayscale: bool,
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.max(0.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Glocke um `center` mit Halbbreite `width`, 1 in der Mitte, 0 am Rand.
fn bump(x: f32, center: f32, width: f32) -> f32 {
    let t = ((x - center) / width).clamp(-1.0, 1.0);
    let v = 1.0 - t * t;
    v * v
}

pub fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    let h = if d < 1e-6 {
        0.0
    } else if max == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    [h, if max > 0.0 { d / max } else { 0.0 }, max]
}

pub fn hsv_to_rgb(h: [f32; 3]) -> [f32; 3] {
    let (hue, s, v) = (h[0].rem_euclid(360.0), h[1].clamp(0.0, 1.0), h[2]);
    let c = v * s;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

fn tint_rgb(t: Tint) -> [f32; 3] {
    hsv_to_rgb([t.hue, 1.0, 1.0])
}

impl Params {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Ein sRGB-Farbwert (0..=1) durch alle Stufen.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let mut c = c;

        // Belichtung und Weißabgleich linear, wie eine Kamera sie sieht.
        if self.exposure != 0.0 || self.temperature != 0.0 || self.tint != 0.0 {
            let gain = 2f32.powf(self.exposure);
            let t = self.temperature / 100.0 * 0.15;
            let g = self.tint / 100.0 * 0.10;
            let wb = [1.0 + t, 1.0 - g, 1.0 - t];
            c = std::array::from_fn(|i| linear_to_srgb(srgb_to_linear(c[i]) * gain * wb[i]));
        }

        // Tonwerte auf die Luminanz, Verhältnis der Kanäle bleibt.
        let l = luma(c);
        let mut nl = l;
        nl += self.contrast / 100.0 * 0.12 * (std::f32::consts::TAU * (l - 0.5)).sin();
        nl += self.shadows / 100.0 * 0.18 * bump(l, 0.25, 0.25);
        nl += self.highlights / 100.0 * 0.18 * bump(l, 0.75, 0.25);
        nl += self.blacks / 100.0 * 0.10 * bump(l, 0.0, 0.2);
        nl += self.whites / 100.0 * 0.10 * bump(l, 1.0, 0.2);
        if !self.curve.is_identity() {
            nl = self.curve.eval(nl.clamp(0.0, 1.0));
        }
        if (nl - l).abs() > 1e-6 {
            if l > 1e-4 {
                let k = nl / l;
                c = c.map(|v| v * k);
            } else {
                c = [nl; 3];
            }
        }
        for (i, curve) in self.curve_rgb.iter().enumerate() {
            if !curve.is_identity() {
                c[i] = curve.eval(c[i].clamp(0.0, 1.0));
            }
        }

        // HSL je Farbbereich, weich überblendet.
        if self.hsl.iter().any(|v| v.iter().any(|x| *x != 0.0)) {
            let mut hsv = rgb_to_hsv(c);
            let (mut dh, mut ds, mut dl) = (0.0, 0.0, 0.0);
            for (k, center) in HSL_CENTERS.iter().enumerate() {
                let dist = ((hsv[0] - center + 180.0).rem_euclid(360.0) - 180.0).abs();
                let w = bump(dist, 0.0, 45.0) * hsv[1].min(1.0);
                dh += w * self.hsl[k][0] / 100.0 * 30.0;
                ds += w * self.hsl[k][1] / 100.0;
                dl += w * self.hsl[k][2] / 100.0 * 0.3;
            }
            hsv[0] += dh;
            hsv[1] *= 1.0 + ds;
            hsv[2] *= 1.0 + dl;
            c = hsv_to_rgb(hsv);
        }

        // Sättigung und Dynamik (Dynamik schont bereits satte Farben).
        if self.grayscale {
            c = [luma(c); 3];
        } else if self.saturation != 0.0 || self.vibrance != 0.0 {
            let l = luma(c);
            let sat_now = rgb_to_hsv(c)[1];
            let k =
                (1.0 + self.saturation / 100.0) * (1.0 + self.vibrance / 100.0 * (1.0 - sat_now));
            c = c.map(|v| l + (v - l) * k.max(0.0));
        }

        // Split-Toning: Tiefen und Lichter getrennt einfärben.
        for (tint, side) in [(self.split_shadows, -1.0f32), (self.split_highlights, 1.0)] {
            if tint.saturation <= 0.0 {
                continue;
            }
            let l = luma(c);
            let pivot = 0.5 - self.split_balance / 100.0 * 0.3;
            let w = if side < 0.0 {
                (1.0 - l / pivot).clamp(0.0, 1.0)
            } else {
                ((l - pivot) / (1.0 - pivot)).clamp(0.0, 1.0)
            };
            let t = tint_rgb(tint);
            let amount = tint.saturation / 100.0 * 0.35 * w;
            // Einfärben bei gleicher Helligkeit.
            let tl = luma(t);
            c = std::array::from_fn(|i| c[i] + (t[i] - tl) * amount);
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        let p = Params::default();
        for c in [[0.0, 0.0, 0.0], [0.2, 0.5, 0.8], [1.0, 1.0, 1.0]] {
            let o = p.apply(c);
            for i in 0..3 {
                assert!((o[i] - c[i]).abs() < 1e-5, "{c:?} -> {o:?}");
            }
        }
    }

    #[test]
    fn exposure_one_stop_doubles_linear() {
        let p = Params {
            exposure: 1.0,
            ..Default::default()
        };
        let o = p.apply([0.5, 0.5, 0.5]);
        let lin = srgb_to_linear(o[0]);
        assert!((lin - 2.0 * srgb_to_linear(0.5)).abs() < 1e-3);
    }

    #[test]
    fn curve_is_monotone_and_hits_points() {
        let c = Curve {
            points: vec![(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)],
        };
        assert!((c.eval(0.25) - 0.15).abs() < 1e-5);
        let mut last = -1.0;
        for i in 0..=100 {
            let y = c.eval(i as f32 / 100.0);
            assert!(y >= last);
            last = y;
        }
    }

    #[test]
    fn grayscale_has_no_color() {
        let p = Params {
            grayscale: true,
            ..Default::default()
        };
        let o = p.apply([0.9, 0.2, 0.1]);
        assert!((o[0] - o[1]).abs() < 1e-6 && (o[1] - o[2]).abs() < 1e-6);
    }

    #[test]
    fn hsl_shifts_only_its_range() {
        let mut p = Params::default();
        p.hsl[5][1] = -100.0; // Blau entsättigen
        let blue = p.apply([0.1, 0.2, 0.9]);
        let red = p.apply([0.9, 0.1, 0.1]);
        assert!(rgb_to_hsv(blue)[1] < 0.2);
        assert!((red[0] - 0.9).abs() < 1e-3);
    }

    #[test]
    fn hsv_roundtrip() {
        for c in [
            [0.9, 0.2, 0.1],
            [0.1, 0.8, 0.3],
            [0.2, 0.3, 0.7],
            [0.5, 0.5, 0.5],
        ] {
            let o = hsv_to_rgb(rgb_to_hsv(c));
            for i in 0..3 {
                assert!((o[i] - c[i]).abs() < 1e-5);
            }
        }
    }
}
