//! Looks: color and tone presets, film simulations, LUTs.
//!
//! Every look is turned into a 33³ LUT for use. Per pixel that leaves
//! a single lookup, whether the look came from a `.cube`,
//! a HaldCLUT or a Lightroom preset with thirty sliders.
//! Grain and vignette run afterwards because they depend on position.

pub mod cube;
pub mod effects;
pub mod hald;
pub mod lut;
pub mod params;
pub mod xmp;

use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;

use crate::decode::Pixels;
use crate::error::{CoreError, Result};
pub use effects::{Grain, Vignette};
pub use lut::Lut3d;
pub use params::{Curve, Params, Tint};

/// Grid size of the baked LUT. 33 is the usual compromise of Resolve
/// and Photoshop; finer isn't visible with 8-bit output.
pub const BAKE_SIZE: usize = 33;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Builtin,
    Cube,
    Hald,
    Xmp,
}

#[derive(Clone, Debug)]
pub struct Look {
    pub name: String,
    pub kind: Kind,
    pub params: Params,
    pub lut: Option<Arc<Lut3d>>,
    pub grain: Grain,
    pub vignette: Vignette,
    /// What couldn't be imported (only for `.xmp`).
    pub ignored: Vec<String>,
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().replace(['_', '-'], " "))
        .unwrap_or_default()
}

impl Look {
    fn plain(name: &str, kind: Kind) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            params: Params::default(),
            lut: None,
            grain: Grain::default(),
            vignette: Vignette::default(),
            ignored: Vec::new(),
        }
    }

    pub fn is_look_file(path: &Path) -> bool {
        matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("cube" | "png" | "xmp")
        )
    }

    /// Loads `.cube`, HaldCLUT `.png` or Lightroom `.xmp`.
    pub fn load(path: &Path) -> Result<Self> {
        let bad = |reason: String| CoreError::Decode {
            path: path.to_owned(),
            reason,
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("cube") => {
                let text = std::fs::read_to_string(path).map_err(|e| CoreError::io(path, e))?;
                let c = cube::parse(&text).map_err(bad)?;
                let mut l = Self::plain(
                    &c.title
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| stem(path)),
                    Kind::Cube,
                );
                l.lut = Some(Arc::new(c.lut));
                Ok(l)
            }
            Some("png") => {
                let img = image::open(path).map_err(|e| CoreError::image(path, e))?;
                let mut l = Self::plain(&stem(path), Kind::Hald);
                l.lut = Some(Arc::new(hald::from_image(&img).map_err(bad)?));
                Ok(l)
            }
            Some("xmp") => {
                let text = std::fs::read_to_string(path).map_err(|e| CoreError::io(path, e))?;
                let p = xmp::parse(&text).map_err(bad)?;
                let mut l = Self::plain(&p.name.unwrap_or_else(|| stem(path)), Kind::Xmp);
                l.params = p.params;
                l.grain = p.grain;
                l.vignette = p.vignette;
                l.ignored = p.ignored;
                Ok(l)
            }
            _ => Err(CoreError::Unsupported(path.to_owned())),
        }
    }

    /// Bundled looks. Deliberately plain and built in-house: film simulations
    /// of well-known brands have unclear rights and are imported, not shipped.
    pub fn builtins() -> Vec<Self> {
        let mut bw = Self::plain("Black & White", Kind::Builtin);
        bw.params.grayscale = true;
        bw.params.contrast = 15.0;

        let mut warm = Self::plain("Warm", Kind::Builtin);
        warm.params.temperature = 35.0;
        warm.params.vibrance = 10.0;

        let mut cool = Self::plain("Cool", Kind::Builtin);
        cool.params.temperature = -30.0;
        cool.params.highlights = -10.0;

        let mut faded = Self::plain("Faded", Kind::Builtin);
        faded.params.curve = Curve {
            points: vec![(0.0, 0.10), (0.5, 0.52), (1.0, 0.94)],
        };
        faded.params.saturation = -25.0;
        faded.params.split_shadows = Tint {
            hue: 200.0,
            saturation: 15.0,
        };

        let mut film = Self::plain("Grainy", Kind::Builtin);
        film.params.contrast = 10.0;
        film.grain = Grain {
            amount: 35.0,
            size: 30.0,
            roughness: 55.0,
        };
        film.vignette = Vignette {
            amount: -12.0,
            midpoint: 50.0,
        };

        vec![bw, warm, cool, faded, film]
    }

    /// Bakes the look with `strength` (0..=1) for use.
    pub fn render(&self, strength: f32) -> Render {
        let s = strength.clamp(0.0, 1.0);
        let identity = self.params.is_identity() && self.lut.is_none();
        let lut = (!identity && s > 0.0).then(|| {
            Lut3d::from_fn(BAKE_SIZE, |c| {
                let a = self.params.apply(c);
                let b = self.lut.as_ref().map_or(a, |l| l.sample(a));
                std::array::from_fn(|i| c[i] + (b[i] - c[i]) * s)
            })
        });
        Render {
            lut,
            grain: Grain {
                amount: self.grain.amount * s,
                ..self.grain
            },
            vignette: Vignette {
                amount: self.vignette.amount * s,
                ..self.vignette
            },
        }
    }
}

/// A finished baked look, cheap to clone and thread-safe.
#[derive(Clone, Debug, Default)]
pub struct Render {
    pub lut: Option<Lut3d>,
    pub grain: Grain,
    pub vignette: Vignette,
}

impl Render {
    pub fn is_identity(&self) -> bool {
        self.lut.is_none() && self.grain.is_none() && self.vignette.is_none()
    }

    /// `seed` fixes the grain. Same file, same grain: the app uses
    /// a hash of the path.
    pub fn apply(&self, px: &mut Pixels, seed: u64) {
        if let Some(lut) = &self.lut {
            px.data.par_chunks_mut(4 * 4096).for_each(|chunk| {
                for p in chunk.chunks_exact_mut(4) {
                    let o = lut.sample([
                        p[0] as f32 / 255.0,
                        p[1] as f32 / 255.0,
                        p[2] as f32 / 255.0,
                    ]);
                    for i in 0..3 {
                        p[i] = (o[i] * 255.0).round() as u8;
                    }
                }
            });
        }
        self.grain.apply(&mut px.data, px.width, seed);
        self.vignette.apply(&mut px.data, px.width, px.height);
    }

    /// The color effect as `.cube` (without grain and vignette, which no LUT format can hold).
    pub fn to_cube(&self, title: &str) -> String {
        self.lut
            .clone()
            .unwrap_or_else(|| Lut3d::identity(BAKE_SIZE))
            .to_cube(title)
    }
}

/// Finds a look for a sidecar key (path in the look folder or
/// `blick:<Name>`). Only the app knows the look folder; it registers it.
pub type Resolver = dyn Fn(&str) -> Option<Arc<Look>> + Send + Sync;

static RESOLVER: std::sync::OnceLock<Box<Resolver>> = std::sync::OnceLock::new();

pub fn set_resolver(f: impl Fn(&str) -> Option<Arc<Look>> + Send + Sync + 'static) {
    let _ = RESOLVER.set(Box::new(f));
}

/// Bundled looks resolve without the app.
fn resolve(key: &str) -> Option<Arc<Look>> {
    if let Some(name) = key.strip_prefix("blick:") {
        // Sidecars written before the English names still carry the German ones.
        let name = match name {
            "Schwarzweiß" => "Black & White",
            "Kühl" => "Cool",
            "Verblasst" => "Faded",
            "Körnig" => "Grainy",
            other => other,
        };
        return Look::builtins()
            .into_iter()
            .find(|l| l.name == name)
            .map(Arc::new);
    }
    RESOLVER.get().and_then(|f| f(key))
}

/// Baked looks for thumbnails, so not every image bakes again.
type RenderCache = std::sync::Mutex<std::collections::HashMap<(String, u32), Arc<Render>>>;
static RENDERS: std::sync::LazyLock<RenderCache> = std::sync::LazyLock::new(Default::default);

/// Applies remembered crop, adjustments and look from `<image>.xmp`, if present.
pub fn apply_sidecar(path: &Path, px: &mut Pixels) {
    let Some(e) = crate::sidecar::read_edit(path) else {
        return;
    };
    if let Some(c) = e.crop {
        *px = c.apply(px);
    }
    crate::adjust::apply(
        &mut px.data,
        crate::Adjustments {
            brightness: e.brightness,
            contrast: e.contrast,
            saturation: e.saturation,
        },
    );
    let Some(key) = e.look else { return };
    let pct = (e.look_strength * 100.0).round() as u32;
    let cached = RENDERS.lock().unwrap().get(&(key.clone(), pct)).cloned();
    let render = match cached {
        Some(r) => r,
        None => {
            let Some(look) = resolve(&key) else { return };
            let r = Arc::new(look.render(e.look_strength));
            RENDERS.lock().unwrap().insert((key, pct), r.clone());
            r
        }
    };
    render.apply(px, seed_for(path));
}

pub fn seed_for(path: &Path) -> u64 {
    let h = blake3::hash(path.as_os_str().as_encoded_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().expect("8 Bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_look_changes_nothing() {
        let r = Look::plain("x", Kind::Builtin).render(1.0);
        assert!(r.is_identity());
        let mut px = Pixels {
            width: 2,
            height: 1,
            data: vec![10, 20, 30, 255, 200, 100, 50, 255],
        };
        let before = px.clone();
        r.apply(&mut px, 1);
        assert_eq!(px, before);
    }

    #[test]
    fn strength_zero_is_identity_and_half_is_between() {
        let bw = &Look::builtins()[0];
        assert!(bw.render(0.0).is_identity());
        let full = bw.render(1.0).lut.unwrap().sample([0.9, 0.1, 0.1]);
        let half = bw.render(0.5).lut.unwrap().sample([0.9, 0.1, 0.1]);
        assert!(half[0] < 0.9 && half[0] > full[0]);
    }

    #[test]
    fn cube_file_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let r = Look::builtins()[3].render(1.0);
        let path = d.path().join("verblasst.cube");
        std::fs::write(&path, r.to_cube("Verblasst")).unwrap();
        let back = Look::load(&path).unwrap();
        assert_eq!(back.name, "Verblasst");
        let (a, b) = (
            r.lut.unwrap().sample([0.4, 0.5, 0.6]),
            back.render(1.0).lut.unwrap().sample([0.4, 0.5, 0.6]),
        );
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 0.01, "{a:?} {b:?}");
        }
    }
}
