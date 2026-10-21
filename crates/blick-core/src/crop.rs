//! Zuschneiden.
//!
//! Der Zuschnitt ist ein Rechteck in Bruchteilen der Bildkante (0..=1), im
//! gedrehten Bild gemessen. So passt er zur Vorschau genauso wie zum Original
//! und lässt sich mit dem Bild drehen. Für die Oberfläche rechnet [`Rect`] in
//! Bildpixeln, dort lassen sich Seitenverhältnisse exakt halten.

use crate::decode::Pixels;
use crate::rotate::Rotation;

/// Kleinster Zuschnitt in Bildpixeln, damit ein Griff nicht im Nichts verschwindet.
pub const MIN_PX: f64 = 16.0;

/// Zuschnitt in Bruchteilen der Bildkante.
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

    /// Ganzes Bild (bis auf Rundungsrauschen aus dem Sidecar).
    pub fn is_identity(&self) -> bool {
        const E: f64 = 1e-4;
        self.x.abs() < E && self.y.abs() < E && (self.w - 1.0).abs() < E && (self.h - 1.0).abs() < E
    }

    /// In den gültigen Bereich holen. `None`, wenn nichts Sinnvolles übrig bleibt.
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

    /// Pixelrechteck `(x, y, w, h)` in einem Bild der Größe `width` x `height`,
    /// immer mindestens 1x1 und ganz im Bild.
    pub fn to_pixels(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let (fw, fh) = (width as f64, height as f64);
        let x0 = (self.x * fw).round().clamp(0.0, fw - 1.0) as u32;
        let y0 = (self.y * fh).round().clamp(0.0, fh - 1.0) as u32;
        let x1 = ((self.x + self.w) * fw).round().clamp(0.0, fw) as u32;
        let y1 = ((self.y + self.h) * fh).round().clamp(0.0, fh) as u32;
        (x0, y0, x1.max(x0 + 1) - x0, y1.max(y0 + 1) - y0)
    }

    /// Größe des Ergebnisses bei einem Bild von `width` x `height`.
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

    /// Derselbe Ausschnitt, nachdem das Bild um 90° gedreht wurde.
    pub fn rotated(&self, rot: Rotation) -> Crop {
        match rot {
            // Ein Punkt (x, y) landet rechtsherum bei (1 − y, x).
            Rotation::Right => Crop {
                x: 1.0 - (self.y + self.h),
                y: self.x,
                w: self.h,
                h: self.w,
            },
            // Linksherum bei (y, 1 − x).
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

/// Seitenverhältnis für den Zuschnitt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Free,
    /// Wie das Bild selbst.
    Original,
    /// Breite : Höhe im Querformat, also immer `a >= b`.
    Ratio(u32, u32),
}

/// Vorlagen in der Reihenfolge der Auswahlliste.
pub const PRESETS: &[(&str, Aspect)] = &[
    ("Frei", Aspect::Free),
    ("Original", Aspect::Original),
    ("Quadrat (1:1)", Aspect::Ratio(1, 1)),
    ("4:3", Aspect::Ratio(4, 3)),
    ("3:2 (Kleinbild)", Aspect::Ratio(3, 2)),
    ("16:9", Aspect::Ratio(16, 9)),
    ("5:4", Aspect::Ratio(5, 4)),
    ("7:5 (13 × 18)", Aspect::Ratio(7, 5)),
    ("2:1 (Panorama)", Aspect::Ratio(2, 1)),
];

impl Aspect {
    /// Breite durch Höhe in Bildpixeln; `None` bei frei. `portrait` stellt
    /// eine Vorlage hochkant, beim Original die gekippte Bildform.
    pub fn ratio(self, img_w: f64, img_h: f64, portrait: bool) -> Option<f64> {
        let r = match self {
            Aspect::Free => return None,
            Aspect::Original => img_w / img_h,
            Aspect::Ratio(a, b) => a as f64 / b as f64,
        };
        let landscape = r.max(1.0 / r);
        Some(if portrait { 1.0 / landscape } else { landscape })
    }

    /// Ob Hoch/Quer bei diesem Verhältnis einen Unterschied macht.
    pub fn has_orientation(self) -> bool {
        !matches!(self, Aspect::Free | Aspect::Ratio(1, 1))
    }
}

/// Rechteck in Bildpixeln (Gleitkomma, damit Ziehen nicht ruckelt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Was gerade gezogen wird.
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
    /// Richtung der bewegten Kanten: −1 links/oben, +1 rechts/unten, 0 fest.
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

    /// Größtes Rechteck mit `ratio` im Bild, möglichst um die Mitte von `self`.
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

    /// Ins Bild schieben (Größe bleibt, solange sie passt).
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

    /// Ergebnis, wenn `handle` um `(dx, dy)` Bildpixel gezogen wird, ausgehend
    /// von `self` (dem Stand beim Anfassen). Mit `ratio` bleibt das
    /// Verhältnis, die gegenüberliegende Ecke bzw. Kante steht fest.
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
        // Fester Bezugspunkt und Platz bis zum Bildrand in Zugrichtung.
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
                // Bei Ecken gewinnt die Achse, die weiter gezogen wurde; bei
                // Kanten folgt die andere Achse.
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
        // Winzig und am Rand: immer noch 1x1 im Bild.
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
        // Vier Mal rechts ist wieder dasselbe.
        let mut r = c;
        for _ in 0..4 {
            r = r.rotated(Rotation::Right);
        }
        assert!(close(r.x, c.x) && close(r.y, c.y));
        // Links oben im Querformat liegt nach Rechtsdrehung rechts oben.
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
        // 4x2-Bild, Pixelwert = Index. Zuschnitt rechte Hälfte, dann drehen:
        // Erst schneiden, dann drehen muss dasselbe ergeben wie drehen, dann
        // den gedrehten Zuschnitt anwenden.
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
        // Original eines Hochkantbilds bleibt hochkant, wenn man es so will.
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
        // Um einen Ausschnitt am Rand: bleibt im Bild.
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
        // Über den Rand hinaus: hält am Rand.
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
        // Ganz zusammengeschoben: Mindestgröße, nicht umgeklappt.
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
        // Gegenüberliegende Ecke steht beim Ziehen an SE.
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
