//! Zwei Caches: dekodierte Vorschauen im RAM, Thumbnails auf der Platte.

use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use image::ImageFormat;
use lru::LruCache;

use crate::decode::{Pixels, load_preview};
use crate::error::{CoreError, Result};

/// LRU mit Byte-Budget statt Eintragszahl: ein 24-MP-Foto und ein Icon
/// sollen nicht gleich viel Platz zählen.
pub struct MemCache<K: Hash + Eq> {
    inner: Mutex<Inner<K>>,
    budget: usize,
}

struct Inner<K: Hash + Eq> {
    map: LruCache<K, Arc<Pixels>>,
    bytes: usize,
}

impl<K: Hash + Eq> MemCache<K> {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: LruCache::unbounded(),
                bytes: 0,
            }),
            budget: budget_bytes,
        }
    }

    pub fn get(&self, key: &K) -> Option<Arc<Pixels>> {
        self.inner.lock().unwrap().map.get(key).cloned()
    }

    pub fn insert(&self, key: K, px: Arc<Pixels>) {
        let mut inner = self.inner.lock().unwrap();
        inner.bytes += px.byte_len();
        if let Some(old) = inner.map.put(key, px) {
            inner.bytes -= old.byte_len();
        }
        while inner.bytes > self.budget && inner.map.len() > 1 {
            if let Some((_, evicted)) = inner.map.pop_lru() {
                inner.bytes -= evicted.byte_len();
            }
        }
    }

    pub fn remove_where(&self, mut pred: impl FnMut(&K) -> bool)
    where
        K: Clone,
    {
        let mut inner = self.inner.lock().unwrap();
        let keys: Vec<K> = inner
            .map
            .iter()
            .filter(|(k, _)| pred(k))
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            if let Some(v) = inner.map.pop(&k) {
                inner.bytes -= v.byte_len();
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.inner.lock().unwrap().bytes
    }
}

/// Thumbnails als PNG unter `$XDG_CACHE_HOME/blick/thumbs/<größe>/`.
/// Der Name hängt an Pfad, Änderungszeit und Dateigröße, eine geänderte
/// Datei bekommt also automatisch ein neues Thumbnail.
pub struct ThumbStore {
    root: PathBuf,
}

impl ThumbStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn key(path: &Path, size: u32) -> Result<PathBuf> {
        let meta = std::fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        let mut h = blake3::Hasher::new();
        h.update(path.as_os_str().as_encoded_bytes());
        h.update(&mtime.as_nanos().to_le_bytes());
        h.update(&meta.len().to_le_bytes());
        // Drehung (RAW) und gemerkte Korrekturen liegen in Sidecars; ändern die
        // sich, braucht es ein neues Thumbnail, obwohl das Bild gleich bleibt.
        for side in [
            crate::sidecar::path_for(path),
            crate::sidecar::edit_path(path),
        ] {
            if let Some(t) = std::fs::metadata(&side)
                .ok()
                .and_then(|m| m.modified().ok())
            {
                h.update(
                    &t.duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                        .to_le_bytes(),
                );
            }
        }
        Ok(PathBuf::from(size.to_string()).join(format!("{}.png", h.finalize().to_hex())))
    }

    pub fn get_or_create(&self, path: &Path, size: u32) -> Result<Pixels> {
        let file = self.root.join(Self::key(path, size)?);
        if let Ok(img) = image::open(&file) {
            return Ok(Pixels::from_dynamic(img));
        }
        let mut px = load_preview(path, size, size)?;
        crate::look::apply_sidecar(path, &mut px);
        // Ein voller Cache-Ordner soll das Anzeigen nicht verhindern.
        if let Err(e) = self.store(&file, &px) {
            tracing::debug!("Thumbnail nicht gespeichert: {e}");
        }
        Ok(px)
    }

    fn store(&self, file: &Path, px: &Pixels) -> Result<()> {
        let dir = file.parent().expect("key hat immer einen Ordner");
        std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
        let img = px.clone().into_dynamic();
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| CoreError::image(file, e))?;
        crate::encode::write_atomic(file, buf.get_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(n: usize) -> Arc<Pixels> {
        Arc::new(Pixels {
            width: n as u32,
            height: 1,
            data: vec![0; n * 4],
        })
    }

    #[test]
    fn evicts_by_bytes() {
        let c = MemCache::new(100);
        c.insert(1, px(10)); // 40 B
        c.insert(2, px(10)); // 80 B
        c.get(&1);
        c.insert(3, px(10)); // 120 B, 2 ist am längsten unbenutzt
        assert!(c.get(&1).is_some());
        assert!(c.get(&2).is_none());
        assert!(c.get(&3).is_some());
        assert_eq!(c.bytes(), 80);
    }

    #[test]
    fn oversized_single_entry_stays() {
        let c = MemCache::new(10);
        c.insert(1, px(100));
        assert!(c.get(&1).is_some());
    }

    #[test]
    fn remove_where_updates_bytes() {
        let c = MemCache::new(1000);
        c.insert((1, 'a'), px(10));
        c.insert((1, 'b'), px(10));
        c.insert((2, 'a'), px(10));
        c.remove_where(|k| k.0 == 1);
        assert_eq!(c.bytes(), 40);
    }

    #[test]
    fn thumb_store_hits_disk_cache() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.png");
        image::DynamicImage::new_rgb8(400, 200).save(&src).unwrap();
        let store = ThumbStore::new(dir.path().join("cache"));
        let a = store.get_or_create(&src, 100).unwrap();
        assert_eq!((a.width, a.height), (100, 50));
        let cached = std::fs::read_dir(dir.path().join("cache/100"))
            .unwrap()
            .count();
        assert_eq!(cached, 1);
        assert_eq!(store.get_or_create(&src, 100).unwrap(), a);

        // Gemerkte Korrektur: neues Thumbnail, entsättigt.
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            40,
            20,
            image::Rgb([200, 30, 30]),
        ))
        .save(&src)
        .unwrap();
        let before = store.get_or_create(&src, 100).unwrap();
        crate::sidecar::write_edit(
            &src,
            &crate::sidecar::EditSettings {
                saturation: -1.0,
                look_strength: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
        let after = store.get_or_create(&src, 100).unwrap();
        assert_ne!(before, after);
        assert!(after.data[0] == after.data[1] && after.data[1] == after.data[2]);
    }
}
