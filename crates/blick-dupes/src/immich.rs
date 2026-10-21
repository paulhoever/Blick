//! Immich als Quelle: Liste samt SHA-1 aus der Suche, Vorschaubilder für den
//! Bildvergleich, Papierkorb statt Löschen. Kein Download für Inhaltsvergleich.

use std::sync::atomic::{AtomicBool, Ordering};

use blick_immich::{Client, ThumbSize};

use crate::error::{DupeError, Result};
use crate::source::{Entry, Storage};

pub struct ImmichStorage {
    pub client: Client,
    pub handle: tokio::runtime::Handle,
    /// `None`: die ganze Bibliothek.
    pub album: Option<String>,
    pub label: String,
}

fn asset_id(e: &Entry) -> Result<&str> {
    e.uri
        .strip_prefix("immich://")
        .and_then(|r| r.split('/').next())
        .ok_or_else(|| DupeError::Other(format!("keine Immich-Adresse: {}", e.uri)))
}

fn err(e: blick_immich::Error) -> DupeError {
    DupeError::Other(format!("Immich: {e}"))
}

impl Storage for ImmichStorage {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn list(&self, cancel: &AtomicBool, on_entry: &mut dyn FnMut(Entry)) -> Result<()> {
        let mut next: Option<String> = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(DupeError::Cancelled);
            }
            let (items, n) = self
                .handle
                .block_on(self.client.search_page(self.album.as_deref(), next.as_deref()))
                .map_err(err)?;
            for a in items {
                on_entry(Entry {
                    uri: format!("immich://{}/{}", a.id, a.name),
                    size: a.size,
                    mtime: a.created_unix,
                    id: None,
                    checksum: (!a.checksum_hex.is_empty()).then_some(a.checksum_hex),
                    dims: a.width.zip(a.height),
                });
            }
            match n {
                Some(n) => next = Some(n),
                None => return Ok(()),
            }
        }
    }

    fn read_range(&self, e: &Entry, offset: u64, len: usize) -> Result<Vec<u8>> {
        let all = self.read_all(e)?;
        let s = (offset as usize).min(all.len());
        Ok(all[s..(s + len).min(all.len())].to_vec())
    }

    fn read_all(&self, e: &Entry) -> Result<Vec<u8>> {
        let id = asset_id(e)?;
        let tmp = tempfile::NamedTempFile::new().map_err(|x| DupeError::io("temp", x))?;
        self.handle
            .block_on(self.client.download_original(id, tmp.path()))
            .map_err(err)?;
        std::fs::read(tmp.path()).map_err(|x| DupeError::io("temp", x))
    }

    fn read_thumb(&self, e: &Entry) -> Result<Vec<u8>> {
        let id = asset_id(e)?;
        self.handle
            .block_on(self.client.thumbnail(id, ThumbSize::Thumbnail))
            .map(|b| b.to_vec())
            .map_err(err)
    }

    fn thumbs_are_rasters(&self) -> bool {
        true
    }

    fn has_cheap_checksums(&self) -> bool {
        true
    }

    /// Immichs Papierkorb: bleibt dort, bis er geleert wird.
    fn trash(&self, e: &Entry) -> Result<()> {
        let id = asset_id(e)?.to_owned();
        self.handle.block_on(self.client.trash_assets(&[id])).map_err(err)
    }
}
