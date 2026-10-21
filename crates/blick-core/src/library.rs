//! Ordner einlesen.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CoreError, Result};

pub fn is_supported(path: &Path) -> bool {
    crate::formats::kind_of(path).is_some()
}

/// Alle Bilder direkt im Ordner (nicht rekursiv), natürlich sortiert:
/// `IMG_2.jpg` kommt vor `IMG_10.jpg`.
pub fn scan_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| CoreError::io(dir, e))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_supported(p))
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
        })
        .collect();
    sort_paths(&mut files);
    Ok(files)
}

/// Ein Ordner mit Bildern innerhalb einer Bibliothek.
#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub dir: PathBuf,
    /// Pfad relativ zur Bibliothek, mit `/` getrennt. Leer für die Wurzel selbst.
    pub rel: String,
    pub count: usize,
    /// Erstes Bild in Ordnerreihenfolge, als Titelbild.
    pub cover: PathBuf,
    /// Alle Bilder des Ordners, natürlich sortiert.
    pub images: Vec<PathBuf>,
}

/// Ordner, die nie Fotos des Nutzers enthalten: Thumbnail-Ablagen von NAS und
/// Dateimanagern, Papierkörbe.
const SKIP_DIRS: &[&str] = &[
    "@eaDir",
    "#recycle",
    "#snapshot",
    "lost+found",
    "$RECYCLE.BIN",
];

fn skip_dir(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy();
    n.starts_with('.') || SKIP_DIRS.iter().any(|s| n.eq_ignore_ascii_case(s))
}

/// Durchläuft `root` rekursiv und meldet jeden Ordner mit Bildern, sobald er
/// gelesen ist. Symlinks auf Ordner werden nicht verfolgt, so gibt es keine
/// Schleifen. Unlesbare Unterordner werden übersprungen, nicht gemeldet.
/// `cancel` bricht zwischen zwei Ordnern ab.
pub fn scan_tree(root: &Path, cancel: &AtomicBool, mut on_album: impl FnMut(Album)) -> Result<()> {
    std::fs::read_dir(root).map_err(|e| CoreError::io(root, e))?;
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(CoreError::Cancelled);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subdirs = Vec::new();
        let mut images = Vec::new();
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if !skip_dir(&entry.file_name()) {
                    subdirs.push(path);
                }
            } else if ft.is_file()
                && is_supported(&path)
                && !entry.file_name().to_string_lossy().starts_with('.')
            {
                images.push(path);
            }
        }
        if !images.is_empty() {
            sort_paths(&mut images);
            let rel = dir
                .strip_prefix(root)
                .map(|r| {
                    r.iter()
                        .map(|c| c.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            on_album(Album {
                cover: images[0].clone(),
                count: images.len(),
                dir: dir.clone(),
                rel,
                images,
            });
        }
        // Rückwärts auf den Stapel, damit alphabetisch zuerst gelesen wird.
        sort_paths(&mut subdirs);
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(())
}

/// Alle Unterordner unter `root` (ohne versteckte, NAS-Ablagen und Symlinks),
/// höchstens `limit` Stück, `root` selbst zuerst. Für die Ordnerüberwachung.
pub fn subdirs(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = vec![root.to_owned()];
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if out.len() >= limit {
                return out;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) && !skip_dir(&e.file_name()) {
                out.push(e.path());
                stack.push(e.path());
            }
        }
    }
    out
}

/// Alle Bilder unter `root` samt Unterordnern: erst die des Ordners selbst,
/// dann Unterordner für Unterordner in alphabetischer Reihenfolge.
pub fn scan_recursive(root: &Path, cancel: &AtomicBool) -> Result<Vec<PathBuf>> {
    let mut all = Vec::new();
    scan_tree(root, cancel, |a| all.extend(a.images))?;
    Ok(all)
}

/// Natürlich sortiert, erst nach Name ohne Endung, damit `foto-bearbeitet.jpg`
/// hinter `foto.jpg` landet.
fn sort_paths(paths: &mut [PathBuf]) {
    let key = |p: &PathBuf| {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = p
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        (stem, ext)
    };
    paths.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        natural_cmp(&ka.0, &kb.0).then_with(|| natural_cmp(&ka.1, &kb.1))
    });
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.next_if(|c| c.is_ascii_digit()) {
                        s.push(c);
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord.is_ne() {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord.is_ne() {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_tree_finds_albums() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        for f in [
            "a.jpg",
            "2024/Urlaub/b.jpg",
            "2024/Urlaub/c.CR3",
            "2024/leer/notes.txt",
            "2023/x.png",
            ".versteckt/y.jpg",
            "2024/@eaDir/z.jpg",
            "2024/Urlaub/Tag 2/d.jpg",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        std::os::unix::fs::symlink(root, root.join("2024/schleife")).unwrap();
        let mut albums = Vec::new();
        scan_tree(root, &AtomicBool::new(false), |a| {
            albums.push((a.rel, a.count))
        })
        .unwrap();
        assert_eq!(
            albums,
            [
                ("".to_owned(), 1),
                ("2023".to_owned(), 1),
                ("2024/Urlaub".to_owned(), 2),
                ("2024/Urlaub/Tag 2".to_owned(), 1)
            ]
        );
    }

    #[test]
    fn scan_recursive_lists_everything_in_order() {
        let d = tempfile::tempdir().unwrap();
        for f in ["z.jpg", "b/2.jpg", "b/10.jpg", "a/x.png", ".h/y.jpg"] {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        let all: Vec<String> = scan_recursive(d.path(), &AtomicBool::new(false))
            .unwrap()
            .iter()
            .map(|p| {
                p.strip_prefix(d.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(all, ["z.jpg", "a/x.png", "b/2.jpg", "b/10.jpg"]);
    }

    #[test]
    fn subdirs_skips_and_limits() {
        let d = tempfile::tempdir().unwrap();
        for f in ["a/b/c", "a/.git", "@eaDir/x", "d"] {
            std::fs::create_dir_all(d.path().join(f)).unwrap();
        }
        let mut all: Vec<String> = subdirs(d.path(), 100)
            .iter()
            .map(|p| {
                p.strip_prefix(d.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        all.sort();
        assert_eq!(all, ["", "a", "a/b", "a/b/c", "d"]);
        assert_eq!(subdirs(d.path(), 2).len(), 2);
    }

    #[test]
    fn scan_tree_cancels() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.jpg"), b"").unwrap();
        let r = scan_tree(d.path(), &AtomicBool::new(true), |_| {});
        assert!(matches!(r, Err(CoreError::Cancelled)));
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["IMG_10.jpg", "img_2.jpg", "IMG_1.jpg", "a.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a.png", "IMG_1.jpg", "img_2.jpg", "IMG_10.jpg"]);
    }

    #[test]
    fn scan_filters_and_sorts() {
        let d = tempfile::tempdir().unwrap();
        for f in [
            "b10.JPG",
            "b9.png",
            "b9-bearbeitet.png",
            "notes.txt",
            ".hidden.jpg",
        ] {
            std::fs::write(d.path().join(f), b"").unwrap();
        }
        std::fs::create_dir(d.path().join("sub.jpg")).unwrap();
        let names: Vec<_> = scan_dir(d.path())
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["b9.png", "b9-bearbeitet.png", "b10.JPG"]);
    }
}
