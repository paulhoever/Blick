//! Brightness, contrast and saturation on RGBA8.
//!
//! Brightness and contrast work per channel through a 256-entry table.
//! Saturation mixes with luminance (Rec. 709). Runs in parallel stripes.

use rayon::prelude::*;

/// All values range from -1.0 ..= 1.0, where 0.0 means unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Adjustments {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

impl Adjustments {
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 0.0 && self.saturation == 0.0
    }

    fn clamped(self) -> Self {
        Self {
            brightness: self.brightness.clamp(-1.0, 1.0),
            contrast: self.contrast.clamp(-1.0, 1.0),
            saturation: self.saturation.clamp(-1.0, 1.0),
        }
    }

    fn lut(&self) -> [u8; 256] {
        // Kontrast: -1 → flach grau, +1 → Faktor 3.
        let factor = if self.contrast >= 0.0 {
            1.0 + 2.0 * self.contrast
        } else {
            1.0 + self.contrast
        };
        let offset = self.brightness * 255.0;
        std::array::from_fn(|i| {
            let v = (i as f32 - 127.5) * factor + 127.5 + offset;
            v.round().clamp(0.0, 255.0) as u8
        })
    }
}

/// Stripe size in pixels: big enough to beat Rayon overhead, small enough to spread evenly.
const CHUNK_PIXELS: usize = 16 * 1024;

pub fn apply(rgba: &mut [u8], adj: Adjustments) {
    let adj = adj.clamped();
    if adj.is_identity() {
        return;
    }
    let lut = adj.lut();
    let sat = 1.0 + adj.saturation;
    let do_sat = adj.saturation != 0.0;

    rgba.par_chunks_mut(CHUNK_PIXELS * 4).for_each(|chunk| {
        for px in chunk.chunks_exact_mut(4) {
            let (mut r, mut g, mut b) = (
                lut[px[0] as usize],
                lut[px[1] as usize],
                lut[px[2] as usize],
            );
            if do_sat {
                let l = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                let mix = |c: u8| (l + (c as f32 - l) * sat).round().clamp(0.0, 255.0) as u8;
                (r, g, b) = (mix(r), mix(g), mix(b));
            }
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        (0..=255u8)
            .flat_map(|v| [v, v.wrapping_mul(3), 255 - v, 128])
            .collect()
    }

    #[test]
    fn identity_is_noop() {
        let mut px = sample();
        apply(&mut px, Adjustments::default());
        assert_eq!(px, sample());
    }

    #[test]
    fn full_brightness_is_white_and_keeps_alpha() {
        let mut px = sample();
        apply(
            &mut px,
            Adjustments {
                brightness: 1.0,
                ..Default::default()
            },
        );
        assert!(
            px.chunks_exact(4)
                .all(|p| p[..3] == [255, 255, 255] && p[3] == 128)
        );
    }

    #[test]
    fn zero_saturation_is_gray() {
        let mut px = sample();
        apply(
            &mut px,
            Adjustments {
                saturation: -1.0,
                ..Default::default()
            },
        );
        assert!(px.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn min_contrast_is_flat() {
        let mut px = sample();
        apply(
            &mut px,
            Adjustments {
                contrast: -1.0,
                ..Default::default()
            },
        );
        assert!(px.chunks_exact(4).all(|p| p[..3] == [128, 128, 128]));
    }

    #[test]
    fn values_out_of_range_are_clamped() {
        let mut a = sample();
        let mut b = sample();
        apply(
            &mut a,
            Adjustments {
                brightness: 5.0,
                ..Default::default()
            },
        );
        apply(
            &mut b,
            Adjustments {
                brightness: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(a, b);
    }
}
