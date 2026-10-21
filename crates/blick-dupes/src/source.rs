//! Source abstraction: local folder, later gvfs locations and Immich.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{DupeError, Result};

/// A file in a source. `uri` is for humans and serves as the cache key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub uri: String,
    pub size: u64,
    pub mtime: i64,
    /// Identity in the file system (`dev:ino`), detects hardlinks.
    pub id: Option<String>,
    /// Checksum supplied by the server (Immich: SHA-1, hex). Saves the download.
    pub checksum: Option<String>,
    /// Dimensions, if the source already knows them.
    pub dims: Option<(u32, u32)>,
}

pub trait Storage: Send + Sync {
    /// Short name for display and cache namespace.
    fn label(&self) -> String;
    /// All files below the root, recursive.
    fn list(&self, cancel: &AtomicBool, on_entry: &mut dyn FnMut(Entry)) -> Result<()>;
    fn read_range(&self, e: &Entry, offset: u64, len: usize) -> Result<Vec<u8>>;
    fn read_all(&self, e: &Entry) -> Result<Vec<u8>>;
    /// Bytes the image can be decoded from for comparison.
    /// Default: the file itself; servers with previews supply the preview.
    fn read_thumb(&self, e: &Entry) -> Result<Vec<u8>> {
        self.read_all(e)
    }
    /// To the trash, where the source has one.
    fn trash(&self, e: &Entry) -> Result<()>;
    /// Sources with a checksum know the content without reading.
    /// Delete permanently, where there is no trash (SMB, SFTP).
    fn delete(&self, e: &Entry) -> Result<()> {
        Err(DupeError::Other(format!("cannot delete: {}", e.uri)))
    }
    /// `read_thumb` returns a raster image that decodes as is, whatever the extension.
    fn thumbs_are_rasters(&self) -> bool {
        false
    }
    /// Path in the local file system, if there is one (moving, hardlinks).
    fn local_path(&self, _e: &Entry) -> Option<PathBuf> {
        None
    }
    fn has_cheap_checksums(&self) -> bool {
        false
    }
}

const SKIP_DIRS: &[&str] = &["@eaDir", "#recycle", "#snapshot", "lost+found", "$RECYCLE.BIN"];

pub fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s))
}

pub struct LocalStorage {
    pub root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(e: &Entry) -> &Path {
        Path::new(&e.uri)
    }
}

impl Storage for LocalStorage {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self, cancel: &AtomicBool, on_entry: &mut dyn FnMut(Entry)) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        std::fs::read_dir(&self.root).map_err(|e| DupeError::io(self.root.display(), e))?;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err(DupeError::Cancelled);
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else { continue };
                let name = entry.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    if !skip_dir(&name) {
                        stack.push(entry.path());
                    }
                } else if ft.is_file() && !name.starts_with('.') {
                    let Ok(md) = entry.metadata() else { continue };
                    on_entry(Entry {
                        uri: entry.path().to_string_lossy().into_owned(),
                        size: md.len(),
                        mtime: md.mtime(),
                        id: Some(format!("{}:{}", md.dev(), md.ino())),
                        checksum: None,
                        dims: None,
                    });
                }
            }
        }
        Ok(())
    }

    fn read_range(&self, e: &Entry, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(Self::path(e)).map_err(|x| DupeError::io(&e.uri, x))?;
        f.seek(SeekFrom::Start(offset)).map_err(|x| DupeError::io(&e.uri, x))?;
        let mut buf = Vec::with_capacity(len);
        f.take(len as u64)
            .read_to_end(&mut buf)
            .map_err(|x| DupeError::io(&e.uri, x))?;
        Ok(buf)
    }

    fn read_all(&self, e: &Entry) -> Result<Vec<u8>> {
        std::fs::read(Self::path(e)).map_err(|x| DupeError::io(&e.uri, x))
    }

    fn local_path(&self, e: &Entry) -> Option<PathBuf> {
        Some(Self::path(e).to_owned())
    }

    /// Locally there is no trash without GIO: the app replaces this
    /// source with the gio route when deleting. This path doesn't delete.
    fn trash(&self, e: &Entry) -> Result<()> {
        Err(DupeError::Other(format!(
            "Trash needs GIO: {}",
            e.uri
        )))
    }
}
