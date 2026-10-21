//! Zuschneiden.
//!
//! The crop is a rectangle in fractions of the image edge (0..=1), measured
//! in the rotated image. That way it fits the preview just as well as the original
//! and rotates along with the picture. For the UI, [`Rect`] works in
//! image pixels, where aspect ratios can be held exactly.

use crate::decode::Pixels;
use crate::rotate::Rotation;

/// Smallest crop in image pixels, so a handle can't vanish into nothing.
pub const MIN_PX: f64 = 16.0;

/// Crop in fractions of the image edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Crop {
    pub const FULL: Crop = Crop {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Whole image (up to rounding noise from the sidecar).
    pub fn is_identity(&self) -> bool {
        const E: f64 = 1e-4;
        self.x.abs() < E && self.y.abs() < E && (self.w - 1.0).abs() < E && (self.h - 1.0).abs() < E
    }

    /// Clamp into the valid range. `None` if nothing sensible is left.
    pub fn sanitized(self) -> Option<Crop> {
        if ![self.x, self.y, self.w, self.h]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = self.w.min(1.0 - x);
        let h = self.h.min(1.0 - y);
        (w > 0.0 && h > 0.0).then_some(Crop { x, y, w, h })
    }

    /// Pixel rectangle `(x, y, w, h)` in an image of size `width` x `height`,
    /// always at least 1x1 and fully inside the image.
    pub fn to_pixels(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let (fw, fh) = (width as f64, height as f64);
        let x0 = (self.x * fw).round().clamp(0.0, fw - 1.0) as u32;
        let y0 = (self.y * fh).round().clamp(0.0, fh - 1.0) as u32;
        let x1 = ((self.x + self.w) * fw).round().clamp(0.0, fw) as u32;
        let y1 = ((self.y + self.h) * fh).round().clamp(0.0, fh) as u32;
        (x0, y0, x1.max(x0 + 1) - x0, y1.max(y0 + 1) - y0)
    }

    /// Size of the result for an image of `width` x `height`.
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        let (_, _, w, h) = self.to_pixels(width, height);
        (w, h)
    }

    pub fn to_rect(&self, width: f64, height: f64) -> Rect {
        Rect {
            x: self.x * width,
            y: self.y * height,
            w: self.w * width,
            h: self.h * height,
        }
    }

    pub fn from_rect(r: Rect, width: f64, height: f64) -> Crop {
        Crop {
            x: r.x / width,
            y: r.y / height,
            w: r.w / width,
            h: r.h / height,
        }
    }

    /// The same section after the image was rotated by 90°.
    pub fn rotated(&self, rot: Rotation) -> Crop {
        match rot {
            // A point (x, y) ends up at (1 − y, x) when turning clockwise.
            Rotation::Right => Crop {
                x: 1.0 - (self.y + self.h),
                y: self.x,
                w: self.h,
                h: self.w,
            },
            // Counterclockwise: (y, 1 − x).
            Rotation::Left => Crop {
                x: self.y,
                y: 1.0 - (self.x + self.w),
                w: self.h,
                h: self.w,
            },
        }
    }

    pub fn apply(&self, px: &Pixels) -> Pixels {
        let (x, y, w, h) = self.to_pixels(px.width, px.height);
        if (x, y, w, h) == (0, 0, px.width, px.height) {
            return px.clone();
        }
        let stride = px.stride();
        let row = w as usize * 4;
        let mut data = Vec::with_capacity(row * h as usize);
        for r in y..y + h {
            let start = r as usize * stride + x as usize * 4;
            data.extend_from_slice(&px.data[start..start + row]);
        }
        Pixels {
            width: w,
            height: h,
            data,
        }
    }
}

/// Aspect ratio for the crop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Free,
    /// Same as the image itself.
    Original,
    /// Width : height in landscape, so always `a >= b`.
    Ratio(u32, u32),
}

/// Presets in the order of the selection list.
pub const PRESETS: &[(&str, Aspect)] = &[
    ("Free", Aspect::Free),
    ("Original", Aspect::Original),
    ("Square (1:1)", Aspect::Ratio(1, 1)),
    ("4:3", Aspect::Ratio(4, 3)),
    ("3:2 (35 mm)", Aspect::Ratio(3, 2)),
    ("16:9", Aspect::Ratio(16, 9)),
    ("5:4", Aspect::Ratio(5, 4)),
    ("7:5 (13 × 18)", Aspect::Ratio(7, 5)),
    ("2:1 (panorama)", Aspect::Ratio(2, 1)),
];

impl Aspect {
    /// Width over height in image pixels; `None` for free. `portrait` flips
    /// a preset to upright; for the original it flips the image shape.
    pub fn ratio(self, img_w: f64, img_h: f64, portrait: bool) -> Option<f64> {
        let r = match self {
            Aspect::Free => return None,
            Aspect::Original => img_w / img_h,
            Aspect::Ratio(a, b) => a as f64 / b as f64,
        };
        let landscape = r.max(1.0 / r);
        Some(if portrait { 1.0 / landscape } else { landscape })
    }

    /// Whether portrait/landscape makes a difference for this ratio.
    pub fn has_orientation(self) -> bool {
        !matches!(self, Aspect::Free | Aspect::Ratio(1, 1))
    }
}

/// Rectangle in image pixels (floating point so dragging doesn't stutter).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What is being dragged right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

impl Handle {
    /// Direction of the moving edges: −1 left/top, +1 right/bottom, 0 fixed.
    fn dirs(self) -> (i8, i8) {
        match self {
            Handle::Move => (0, 0),
            Handle::N => (0, -1),
            Handle::S => (0, 1),
            Handle::E => (1, 0),
            Handle::W => (-1, 0),
            Handle::NE => (1, -1),
            Handle::NW => (-1, -1),
            Handle::SE => (1, 1),
            Handle::SW => (-1, 1),
        }
    }
}

impl Rect {
    pub fn full(w: f64, h: f64) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Largest rectangle with `ratio` inside the image, centered on `self` where possible.
    pub fn fit_ratio(&self, ratio: Option<f64>, img_w: f64, img_h: f64) -> Rect {
        let Some(r) = ratio else {
            return self.clamped(img_w, img_h);
        };
        let (w, h) = if img_w / img_h > r {
            (img_h * r, img_h)
        } else {
            (img_w, img_w / r)
        };
        let (cx, cy) = self.center();
        Rect {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
        }
        .clamped(img_w, img_h)
    }

    /// Push into the image (size stays as long as it fits).
    pub fn clamped(&self, img_w: f64, img_h: f64) -> Rect {
        let w = self.w.min(img_w);
        let h = self.h.min(img_h);
        Rect {
            x: self.x.clamp(0.0, img_w - w),
            y: self.y.clamp(0.0, img_h - h),
            w,
            h,
        }
    }

    /// Verschieben, am Bildrand anhalten.
    pub fn moved(&self, dx: f64, dy: f64, img_w: f64, img_h: f64) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
        .clamped(img_w, img_h)
    }

    /// Result when `handle` is dragged by `(dx, dy)` image pixels, starting
    /// from `self` (the state when grabbed). With `ratio` the ratio is kept
    /// and the opposite corner or edge stays put.
    pub fn dragged(
        &self,
        handle: Handle,
        dx: f64,
        dy: f64,
        ratio: Option<f64>,
        img_w: f64,
        img_h: f64,
    ) -> Rect {
        if handle == Handle::Move {
            return self.moved(dx, dy, img_w, img_h);
        }
        let (sx, sy) = handle.dirs();
        let min = MIN_PX.min(img_w).min(img_h);
        // Fixed anchor point and room to the image edge in the drag direction.
        let (ax, room_x) = match sx {
            1 => (self.x, img_w - self.x),
            -1 => (self.x + self.w, self.x + self.w),
            _ => (
                self.x + self.w / 2.0,
                2.0 * (self.x + self.w / 2.0).min(img_w - self.x - self.w / 2.0),
            ),
        };
        let (ay, room_y) = match sy {
            1 => (self.y, img_h - self.y),
            -1 => (self.y + self.h, self.y + self.h),
            _ => (
                self.y + self.h / 2.0,
                2.0 * (self.y + self.h / 2.0).min(img_h - self.y - self.h / 2.0),
            ),
        };
        let mut w = if sx != 0 {
            self.w + sx as f64 * dx
        } else {
            self.w
        };
        let mut h = if sy != 0 {
            self.h + sy as f64 * dy
        } else {
            self.h
        };
        match ratio {
            None => {
                w = w.clamp(min, room_x.max(min));
                h = h.clamp(min, room_y.max(min));
            }
            Some(r) => {
                // For corners the axis dragged further wins; for
                // edges the other axis follows.
                w = match (sx, sy) {
                    (0, _) => h * r,
                    (_, 0) => w,
                    _ => w.max(h * r),
                };
                let max_w = room_x.min(room_y * r);
                let min_w = min.max(min * r);
                w = w.clamp(min_w.min(max_w), max_w);
                h = w / r;
            }
        }
        let x = match sx {
            1 => ax,
            -1 => ax - w,
            _ => ax - w / 2.0,
        };
        let y = match sy {
            1 => ay,
            -1 => ay - h,
            _ => ay - h / 2.0,
        };
        Rect { x, y, w, h }.clamped(img_w, img_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn inside(r: &Rect, w: f64, h: f64) -> bool {
        r.x >= -1e-9 && r.y >= -1e-9 && r.x + r.w <= w + 1e-9 && r.y + r.h <= h + 1e-9
    }

    #[test]
    fn pixels_are_exact_and_inside() {
        let c = Crop {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        assert_eq!(c.to_pixels(400, 300), (100, 150, 200, 150));
        assert_eq!(Crop::FULL.to_pixels(7, 5), (0, 0, 7, 5));
        // Tiny and at the edge: still 1x1 inside the image.
        let edge = Crop {
            x: 1.0,
            y: 1.0,
            w: 0.0,
            h: 0.0,
        };
        assert_eq!(edge.to_pixels(10, 10), (9, 9, 1, 1));
    }

    #[test]
    fn sanitize_rejects_garbage() {
        assert_eq!(
            Crop {
                x: f64::NAN,
                ..Crop::FULL
            }
            .sanitized(),
            None
        );
        assert_eq!(
            Crop {
                x: 0.5,
                y: 0.0,
                w: 0.9,
                h: 1.0
            }
            .sanitized()
            .unwrap()
            .w,
            0.5
        );
        assert_eq!(
            Crop {
                x: 0.2,
                y: 0.2,
                w: 0.0,
                h: 0.5
            }
            .sanitized(),
            None
        );
    }

    #[test]
    fn rotation_roundtrip_and_direction() {
        let c = Crop {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.4,
        };
        for rot in [Rotation::Left, Rotation::Right] {
            let back = c.rotated(rot).rotated(rot.inverse());
            assert!(
                close(back.x, c.x)
                    && close(back.y, c.y)
                    && close(back.w, c.w)
                    && close(back.h, c.h)
            );
        }
        // Four turns to the right is the identity again.
        let mut r = c;
        for _ in 0..4 {
            r = r.rotated(Rotation::Right);
        }
        assert!(close(r.x, c.x) && close(r.y, c.y));
        // Top left in landscape ends up top right after turning right.
        let tl = Crop {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        }
        .rotated(Rotation::Right);
        assert!(close(tl.x, 0.5) && close(tl.y, 0.0));
    }

    #[test]
    fn rotation_matches_pixels() {
        // 4x2 image, pixel value = index. Crop the right half, then rotate:
        // cropping first and rotating second has to give the same as rotating,
        // then applying the rotated crop.
        let px = Pixels {
            width: 4,
            height: 2,
            data: (0..8u8).flat_map(|i| [i, 0, 0, 255]).collect(),
        };
        let c = Crop {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let cut = c.apply(&px);
        let rot_cut = rotate_right(&cut);
        let rot = rotate_right(&px);
        let cut_rot = c.rotated(Rotation::Right).apply(&rot);
        assert_eq!(rot_cut.data, cut_rot.data);
        assert_eq!((cut_rot.width, cut_rot.height), (2, 2));
    }

    fn rotate_right(px: &Pixels) -> Pixels {
        let img = px.clone().into_dynamic().rotate90();
        Pixels::from_dynamic(img)
    }

    #[test]
    fn apply_cuts_rows() {
        let px = Pixels {
            width: 3,
            height: 3,
            data: (0..9u8).flat_map(|i| [i, i, i, 255]).collect(),
        };
        let out = Crop {
            x: 1.0 / 3.0,
            y: 1.0 / 3.0,
            w: 2.0 / 3.0,
            h: 1.0 / 3.0,
        }
        .apply(&px);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.data[0], 4);
        assert_eq!(out.data[4], 5);
    }

    #[test]
    fn presets_and_orientation() {
        assert_eq!(Aspect::Free.ratio(400.0, 300.0, false), None);
        assert!(close(
            Aspect::Ratio(3, 2).ratio(1.0, 1.0, false).unwrap(),
            1.5
        ));
        assert!(close(
            Aspect::Ratio(3, 2).ratio(1.0, 1.0, true).unwrap(),
            2.0 / 3.0
        ));
        // The original of a portrait image stays portrait if you want it to.
        assert!(close(
            Aspect::Original.ratio(300.0, 400.0, true).unwrap(),
            0.75
        ));
        assert!(close(
            Aspect::Original.ratio(300.0, 400.0, false).unwrap(),
            4.0 / 3.0
        ));
        assert!(!Aspect::Ratio(1, 1).has_orientation());
        assert!(PRESETS.iter().all(|(_, a)| match a {
            Aspect::Ratio(a, b) => a >= b,
            _ => true,
        }));
    }

    #[test]
    fn fit_ratio_is_largest_and_centered() {
        let full = Rect::full(400.0, 300.0);
        let sq = full.fit_ratio(Some(1.0), 400.0, 300.0);
        assert!(close(sq.w, 300.0) && close(sq.h, 300.0) && close(sq.x, 50.0));
        // Around a section at the edge: stays inside the image.
        let corner = Rect {
            x: 350.0,
            y: 0.0,
            w: 50.0,
            h: 50.0,
        };
        let r = corner.fit_ratio(Some(16.0 / 9.0), 400.0, 300.0);
        assert!(inside(&r, 400.0, 300.0));
        assert!(close(r.w / r.h, 16.0 / 9.0));
        assert!(close(r.w, 400.0));
    }

    #[test]
    fn drag_free_moves_one_edge() {
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 100.0,
            h: 100.0,
        };
        let e = r.dragged(Handle::E, 30.0, 999.0, None, 400.0, 300.0);
        assert_eq!(e, Rect { w: 130.0, ..r });
        let nw = r.dragged(Handle::NW, -20.0, -10.0, None, 400.0, 300.0);
        assert_eq!(
            nw,
            Rect {
                x: 80.0,
                y: 90.0,
                w: 120.0,
                h: 110.0
            }
        );
        // Past the edge: stops at the edge.
        let far = r.dragged(Handle::SE, 1000.0, 1000.0, None, 400.0, 300.0);
        assert_eq!(
            far,
            Rect {
                x: 100.0,
                y: 100.0,
                w: 300.0,
                h: 200.0
            }
        );
        // Pushed fully together: minimum size, not flipped.
        let tiny = r.dragged(Handle::W, 500.0, 0.0, None, 400.0, 300.0);
        assert!(close(tiny.w, MIN_PX) && close(tiny.x + tiny.w, 200.0));
    }

    #[test]
    fn drag_keeps_ratio_and_bounds() {
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 90.0,
            h: 60.0,
        };
        for h in [
            Handle::N,
            Handle::S,
            Handle::E,
            Handle::W,
            Handle::NE,
            Handle::NW,
            Handle::SE,
            Handle::SW,
        ] {
            for (dx, dy) in [(40.0, 10.0), (-500.0, 3.0), (800.0, 800.0), (-30.0, -70.0)] {
                let out = r.dragged(h, dx, dy, Some(1.5), 400.0, 300.0);
                assert!(close(out.w / out.h, 1.5), "{h:?} {dx},{dy}: {out:?}");
                assert!(inside(&out, 400.0, 300.0), "{h:?} {dx},{dy}: {out:?}");
                assert!(out.h >= MIN_PX - 1e-9, "{h:?}: {out:?}");
            }
        }
        // Opposite corner stays fixed when dragging at SE.
        let se = r.dragged(Handle::SE, 30.0, 0.0, Some(1.5), 400.0, 300.0);
        assert!(close(se.x, 100.0) && close(se.y, 100.0) && close(se.w, 120.0));
    }

    #[test]
    fn move_stops_at_edges() {
        let r = Rect {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 50.0,
        };
        assert_eq!(
            r.moved(-50.0, 500.0, 400.0, 300.0),
            Rect {
                x: 0.0,
                y: 250.0,
                ..r
            }
        );
    }

    #[test]
    fn rect_roundtrip() {
        let c = Crop {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.4,
        };
        let back = Crop::from_rect(c.to_rect(640.0, 480.0), 640.0, 480.0);
        assert!(close(back.x, c.x) && close(back.h, c.h));
    }
}
