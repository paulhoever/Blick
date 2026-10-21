//! 3D-LUT mit trilinearer Interpolation.

/// Würfel mit `size³` Einträgen, Werte 0..=1. Reihenfolge wie in `.cube` und
/// HaldCLUT: Rot läuft am schnellsten, dann Grün, dann Blau.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    pub size: usize,
    pub data: Vec<[f32; 3]>,
}

impl Lut3d {
    pub fn identity(size: usize) -> Self {
        let n = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / n, g as f32 / n, b as f32 / n]);
                }
            }
        }
        Self { size, data }
    }

    /// Rechnet `f` an jedem Gitterpunkt aus.
    pub fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3] + Sync) -> Self {
        use rayon::prelude::*;
        let id = Self::identity(size);
        let data = id
            .data
            .par_iter()
            .map(|&c| {
                let o = f(c);
                [
                    o[0].clamp(0.0, 1.0),
                    o[1].clamp(0.0, 1.0),
                    o[2].clamp(0.0, 1.0),
                ]
            })
            .collect();
        Self { size, data }
    }

    #[inline]
    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + self.size * (g + self.size * b)]
    }

    #[inline]
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = (self.size - 1) as f32;
        let pos = rgb.map(|c| c.clamp(0.0, 1.0) * n);
        let i0 = pos.map(|p| (p.floor() as usize).min(self.size - 2));
        let f = [
            pos[0] - i0[0] as f32,
            pos[1] - i0[1] as f32,
            pos[2] - i0[2] as f32,
        ];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        let (r, g, b) = (i0[0], i0[1], i0[2]);
        let c00 = lerp(self.at(r, g, b), self.at(r + 1, g, b), f[0]);
        let c10 = lerp(self.at(r, g + 1, b), self.at(r + 1, g + 1, b), f[0]);
        let c01 = lerp(self.at(r, g, b + 1), self.at(r + 1, g, b + 1), f[0]);
        let c11 = lerp(self.at(r, g + 1, b + 1), self.at(r + 1, g + 1, b + 1), f[0]);
        lerp(lerp(c00, c10, f[1]), lerp(c01, c11, f[1]), f[2])
    }

    /// Auf eine andere Gittergröße umrechnen, etwa eine 144³-HaldCLUT auf 33³.
    pub fn resampled(&self, size: usize) -> Self {
        if size == self.size {
            return self.clone();
        }
        Self::from_fn(size, |c| self.sample(c))
    }

    /// Als `.cube`-Text, lesbar von Resolve, Photoshop, Premiere, darktable.
    pub fn to_cube(&self, title: &str) -> String {
        let mut s = format!(
            "TITLE \"{}\"\nLUT_3D_SIZE {}\n",
            title.replace('"', "'"),
            self.size
        );
        for c in &self.data {
            s.push_str(&format!("{:.6} {:.6} {:.6}\n", c[0], c[1], c[2]));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_samples_exactly() {
        let lut = Lut3d::identity(17);
        for c in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.3, 0.6, 0.9],
            [0.123, 0.456, 0.789],
        ] {
            let o = lut.sample(c);
            for i in 0..3 {
                assert!((o[i] - c[i]).abs() < 1e-5, "{c:?} -> {o:?}");
            }
        }
    }

    #[test]
    fn resample_keeps_function() {
        let inv = Lut3d::from_fn(9, |c| [1.0 - c[0], 1.0 - c[1], 1.0 - c[2]]);
        let big = inv.resampled(33);
        let o = big.sample([0.25, 0.5, 0.75]);
        assert!((o[0] - 0.75).abs() < 1e-4 && (o[2] - 0.25).abs() < 1e-4);
    }
}
