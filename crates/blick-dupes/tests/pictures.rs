use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use blick_dupes::hashdb::HashDb;
use blick_dupes::{LocalStorage, Mode, Options, Scanner, SourceSpec};
use image::{DynamicImage, Rgb, RgbImage, imageops::FilterType};

/// Image with structure, depending on the seed: color areas and a gradient.
fn scene(seed: u32, w: u32, h: u32) -> RgbImage {
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(12345);
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let blocks: Vec<[u8; 3]> = (0..36).map(|_| [rnd() as u8, rnd() as u8, rnd() as u8]).collect();
    RgbImage::from_fn(w, h, |x, y| {
        let b = blocks[((y * 6 / h) * 6 + x * 6 / w) as usize];
        Rgb(b)
    })
}

fn save(img: &RgbImage, dir: &std::path::Path, name: &str) {
    img.save(dir.join(name)).unwrap();
}

fn run(dir: &std::path::Path, tweak: impl FnOnce(&mut Options)) -> Vec<blick_dupes::Group> {
    let mut options = Options { mode: Mode::Pictures, ..Options::default() };
    tweak(&mut options);
    Scanner {
        sources: vec![SourceSpec { storage: Arc::new(LocalStorage::new(dir)), reference: false }],
        options,
        db: Arc::new(HashDb::in_memory().unwrap()),
    }
    .run(&AtomicBool::new(false), &|_| {})
    .unwrap()
}

fn names(g: &blick_dupes::Group) -> Vec<String> {
    let mut v: Vec<_> = g.members.iter().map(|m| m.entry.uri.rsplit('/').next().unwrap().to_string()).collect();
    v.sort();
    v
}

#[test]
fn finds_resized_and_recompressed_but_not_different() {
    let d = tempfile::tempdir().unwrap();
    let a = scene(1, 600, 400);
    save(&a, d.path(), "a.png");
    let small = DynamicImage::ImageRgb8(a.clone()).resize_exact(300, 200, FilterType::Triangle).to_rgb8();
    save(&small, d.path(), "a_small.jpg");
    save(&scene(2, 600, 400), d.path(), "other.png");
    let g = run(d.path(), |_| {});
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(names(&g[0]), ["a.png", "a_small.jpg"]);
    assert!(g[0].score >= 95);
}

#[test]
fn rotation_only_with_option() {
    let d = tempfile::tempdir().unwrap();
    let a = scene(3, 600, 400);
    save(&a, d.path(), "a.png");
    save(&DynamicImage::ImageRgb8(a).rotate90().to_rgb8(), d.path(), "a_rot.png");
    assert!(run(d.path(), |_| {}).is_empty());
    let g = run(d.path(), |o| {
        o.ignore_rotation = true;
        o.aspect_tolerance = None;
    });
    assert_eq!(g.len(), 1, "{g:?}");
}

#[test]
fn threshold_controls_strictness() {
    let d = tempfile::tempdir().unwrap();
    let a = scene(4, 600, 400);
    save(&a, d.path(), "a.png");
    let mut b = a.clone();
    for y in 0..400 {
        for x in 0..120 {
            b.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    save(&b, d.path(), "b.png");
    assert!(run(d.path(), |_| {}).is_empty());
    assert_eq!(run(d.path(), |o| o.threshold = 80).len(), 1);
}
