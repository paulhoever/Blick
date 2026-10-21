//! Körnung und Vignette. Beide hängen von der Position im Bild ab und passen
//! deshalb nicht in eine LUT; sie laufen nach ihr.

use rayon::prelude::*;

/// Lightroom-Einheiten: Stärke 0..=100, Größe 0..=100 (Standard 25),
/// Rauheit 0..=100 (Standard 50).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grain {
    pub amount: f32,
    pub size: f32,
    pub roughness: f32,
}

impl Default for Grain {
    fn default() -> Self {
        Self {
            amount: 0.0,
            size: 25.0,
            roughness: 50.0,
        }
    }
}

/// Stärke -100..=100 (negativ dunkelt ab), Mittelpunkt 0..=100 (Standard 50).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vignette {
    pub amount: f32,
    pub midpoint: f32,
}

impl Default for Vignette {
    fn default() -> Self {
        Self {
            amount: 0.0,
            midpoint: 50.0,
        }
    }
}

/// Bezugsbreite: Kornstruktur relativ zur Bildbreite, damit Vorschau und
/// Export in voller Auflösung gleich aussehen.
const GRAIN_REF_WIDTH: f32 = 1600.0;

fn hash(x: i32, y: i32, seed: u64) -> f32 {
    let mut z = seed ^ ((x as u64) << 32 | (y as u32 as u64));
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
}

/// Wertrauschen: Gitterwerte gehasht, dazwischen weich interpoliert.
fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let (ix, iy) = (x0 as i32, y0 as i32);
    let a = hash(ix, iy, seed) + (hash(ix + 1, iy, seed) - hash(ix, iy, seed)) * sx;
    let b = hash(ix, iy + 1, seed) + (hash(ix + 1, iy + 1, seed) - hash(ix, iy + 1, seed)) * sx;
    a + (b - a) * sy
}

impl Grain {
    pub fn is_none(&self) -> bool {
        self.amount <= 0.0
    }

    pub fn apply(&self, rgba: &mut [u8], width: u32, seed: u64) {
        if self.is_none() {
            return;
        }
        let cell = (0.6 + self.size / 25.0) * width as f32 / GRAIN_REF_WIDTH;
        let cell = cell.max(0.5);
        let rough = (self.roughness / 100.0).clamp(0.0, 1.0);
        let amp = self.amount / 100.0 * 0.16 * 255.0;
        let w = width as usize;
        rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let (fx, fy) = (x as f32 / cell, y as f32 / cell);
                let n = value_noise(fx, fy, seed) * (1.0 - rough)
                    + value_noise(fx * 2.3, fy * 2.3, seed ^ 0xA5A5) * rough;
                let l =
                    (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0;
                // Korn sieht man in den Mitteltönen, kaum in Schwarz und Weiß.
                let d = n * amp * (0.25 + 3.0 * l * (1.0 - l));
                for c in &mut px[..3] {
                    *c = (*c as f32 + d).round().clamp(0.0, 255.0) as u8;
                }
            }
        });
    }
}

impl Vignette {
    pub fn is_none(&self) -> bool {
        self.amount == 0.0
    }

    pub fn apply(&self, rgba: &mut [u8], width: u32, height: u32) {
        if self.is_none() {
            return;
        }
        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let start = 0.15 + 0.7 * (self.midpoint / 100.0).clamp(0.0, 1.0);
        let k = self.amount / 100.0;
        let w = width as usize;
        rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            let dy = (y as f32 + 0.5 - cy) / cy;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let dx = (x as f32 + 0.5 - cx) / cx;
                let d = ((dx * dx + dy * dy) / 2.0).sqrt();
                let t = ((d - start) / (1.0 - start).max(1e-3)).clamp(0.0, 1.0);
                let f = 1.0 + k * t * t * (3.0 - 2.0 * t);
                for c in &mut px[..3] {
                    *c = (*c as f32 * f).round().clamp(0.0, 255.0) as u8;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: u32, h: u32) -> Vec<u8> {
        vec![128u8; (w * h * 4) as usize]
    }

    #[test]
    fn grain_is_deterministic_and_seeded() {
        let g = Grain {
            amount: 50.0,
            ..Default::default()
        };
        let (mut a, mut b, mut c) = (gray(64, 64), gray(64, 64), gray(64, 64));
        g.apply(&mut a, 64, 7);
        g.apply(&mut b, 64, 7);
        g.apply(&mut c, 64, 8);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, gray(64, 64));
    }

    #[test]
    fn vignette_darkens_corners_only() {
        let v = Vignette {
            amount: -100.0,
            midpoint: 50.0,
        };
        let mut px = gray(100, 100);
        v.apply(&mut px, 100, 100);
        let at = |x: usize, y: usize| px[(y * 100 + x) * 4];
        assert_eq!(at(50, 50), 128);
        assert!(at(0, 0) < 40);
    }
}
