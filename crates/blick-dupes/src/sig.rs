//! Image signature in the style of dupeGuru ("picture blocks"): 15×15 blocks with
//! mean color. Plus a 64-bit gradient hash as a search key.

use std::path::Path;

use blick_core::decode::{self, Pixels};
use blick_core::formats::{Kind, kind_of};

use crate::error::{DupeError, Result};
use crate::source::{Entry, Storage};

pub const GRID: usize = 15;
pub const GRID_LEN: usize = GRID * GRID * 3;
/// Edge length images are shrunk to before averaging.
const WORK_PX: u32 = 150;
const BLOB_LEN: usize = 8 + GRID_LEN;

#[derive(Clone, PartialEq, Eq)]
pub struct Sig {
    pub width: u32,
    pub height: u32,
    pub grid: [u8; GRID_LEN],
}

impl std::fmt::Debug for Sig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sig({}x{})", self.width, self.height)
    }
}

/// Rows and columns that go into the hash: 8 rows, 9 columns spread
/// across the 15 grid, giving 8×8 horizontal comparisons.
const ROWS: [usize; 8] = [0, 2, 4, 6, 8, 10, 12, 14];
const COLS: [usize; 9] = [0, 2, 3, 5, 7, 9, 11, 12, 14];

impl Sig {
    pub fn to_blob(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(BLOB_LEN);
        v.extend_from_slice(&self.width.to_le_bytes());
        v.extend_from_slice(&self.height.to_le_bytes());
        v.extend_from_slice(&self.grid);
        v
    }

    pub fn from_blob(b: &[u8]) -> Option<Self> {
        if b.len() != BLOB_LEN {
            return None;
        }
        let mut grid = [0u8; GRID_LEN];
        grid.copy_from_slice(&b[8..]);
        Some(Self {
            width: u32::from_le_bytes(b[0..4].try_into().ok()?),
            height: u32::from_le_bytes(b[4..8].try_into().ok()?),
            grid,
        })
    }

    fn lum(grid: &[u8; GRID_LEN], x: usize, y: usize) -> u32 {
        let i = (y * GRID + x) * 3;
        // ganzzahlig: 0,299 R + 0,587 G + 0,114 B
        (grid[i] as u32 * 299 + grid[i + 1] as u32 * 587 + grid[i + 2] as u32 * 114) / 1000
    }

    /// Search key: brightness grows to the right (1) or not (0).
    pub fn hash(&self) -> u64 {
        hash_grid(&self.grid)
    }

    /// Rotated 90° clockwise (grid and dimensions).
    pub fn rotated(&self) -> Sig {
        let mut grid = [0u8; GRID_LEN];
        for y in 0..GRID {
            for x in 0..GRID {
                // Cell (x, y) ends up at (GRID-1-y, x)
                let src = (y * GRID + x) * 3;
                let dst = (x * GRID + (GRID - 1 - y)) * 3;
                grid[dst..dst + 3].copy_from_slice(&self.grid[src..src + 3]);
            }
        }
        Sig { width: self.height, height: self.width, grid }
    }

    /// Similarity 0..=100: 100 minus mean deviation per color value.
    pub fn similarity(&self, other: &Sig) -> f32 {
        let sum: u32 = self
            .grid
            .iter()
            .zip(other.grid.iter())
            .map(|(a, b)| a.abs_diff(*b) as u32)
            .sum();
        100.0 - sum as f32 * 100.0 / (GRID_LEN as f32 * 255.0)
    }

    /// Aspect ratio deviates by at most `tol` (a fraction).
    pub fn same_aspect(&self, other: &Sig, tol: f32) -> bool {
        let a = self.width as f32 / self.height.max(1) as f32;
        let b = other.width as f32 / other.height.max(1) as f32;
        (a / b - 1.0).abs() <= tol
    }
}

pub fn hash_grid(grid: &[u8; GRID_LEN]) -> u64 {
    let mut h = 0u64;
    for &y in &ROWS {
        for w in COLS.windows(2) {
            h <<= 1;
            if Sig::lum(grid, w[1], y) > Sig::lum(grid, w[0], y) {
                h |= 1;
            }
        }
    }
    h
}

/// Mean color per block. Edge blocks count whole pixels, blocks are never empty.
pub fn from_pixels(px: &Pixels, width: u32, height: u32) -> Sig {
    let (w, h) = (px.width as usize, px.height as usize);
    let mut grid = [0u8; GRID_LEN];
    for gy in 0..GRID {
        let y0 = gy * h / GRID;
        let y1 = ((gy + 1) * h / GRID).max(y0 + 1).min(h);
        for gx in 0..GRID {
            let x0 = gx * w / GRID;
            let x1 = ((gx + 1) * w / GRID).max(x0 + 1).min(w);
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    r += px.data[i] as u64;
                    g += px.data[i + 1] as u64;
                    b += px.data[i + 2] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = (gy * GRID + gx) * 3;
            grid[o] = (r / n) as u8;
            grid[o + 1] = (g / n) as u8;
            grid[o + 2] = (b / n) as u8;
        }
    }
    Sig { width, height, grid }
}

/// Signature of a file from any source. Grid straight from the bytes,
/// RAW/HEIC and the like through a temp file (the decoders want a path).
pub fn compute(storage: &dyn Storage, e: &Entry) -> Result<Sig> {
    let path = Path::new(&e.uri);
    let kind = kind_of(path).ok_or_else(|| DupeError::Other(format!("not an image: {}", e.uri)))?;
    let bytes = storage.read_thumb(e)?;
    let (px, ow, oh) = match kind {
        Kind::Raster | Kind::Raw | Kind::Other if storage.thumbs_are_rasters() => {
            let small = decode::load_bytes(&bytes).map_err(|x| DupeError::Other(format!("{}: {x}", e.uri)))?;
            let (ow, oh) = (small.width, small.height);
            (small, ow, oh)
        }
        Kind::Raster => {
            let full = decode::load_bytes(&bytes).map_err(|x| DupeError::Other(format!("{}: {x}", e.uri)))?;
            let (ow, oh) = (full.width, full.height);
            let small = full.fit_within(WORK_PX, WORK_PX).map_err(|x| DupeError::Other(x.to_string()))?;
            (small, ow, oh)
        }
        Kind::Raw | Kind::Other => {
            let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("bin");
            let mut tmp = tempfile::Builder::new()
                .suffix(&format!(".{ext}"))
                .tempfile()
                .map_err(|x| DupeError::io("temp", x))?;
            std::io::Write::write_all(&mut tmp, &bytes).map_err(|x| DupeError::io("temp", x))?;
            let small = decode::load_preview(tmp.path(), WORK_PX, WORK_PX)
                .map_err(|x| DupeError::Other(format!("{}: {x}", e.uri)))?;
            // Dimensions of the preview are enough for the aspect ratio.
            let (w, h) = (small.width, small.height);
            (small, w, h)
        }
        Kind::Video => return Err(DupeError::Other("Video".into())),
    };
    Ok(from_pixels(&px, ow, oh))
}

pub fn is_picture(uri: &str) -> bool {
    matches!(
        kind_of(Path::new(uri)),
        Some(Kind::Raster | Kind::Raw | Kind::Other)
    )
}
