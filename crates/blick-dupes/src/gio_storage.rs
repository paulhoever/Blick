//! Remote locations through GIO/gvfs: `smb://`, `sftp://`, `dav(s)://`, `nfs://`, `file://`.
//! Reads use seek and range, so pre-hashes only touch a few hundred KiB.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use gio::prelude::*;

use crate::error::{DupeError, Result};
use crate::source::{Entry, Storage, skip_dir};

pub struct GioStorage {
    pub root_uri: String,
}

impl GioStorage {
    pub fn new(root_uri: impl Into<String>) -> Self {
        Self { root_uri: root_uri.into() }
    }
}

fn gerr(what: &str, e: gio::glib::Error) -> DupeError {
    DupeError::Other(format!("{what}: {}", e.message()))
}

const ATTRS: &str = "standard::name,standard::type,standard::size,time::modified,unix::inode,unix::device";

impl Storage for GioStorage {
    fn label(&self) -> String {
        self.root_uri.clone()
    }

    fn list(&self, cancel: &AtomicBool, on_entry: &mut dyn FnMut(Entry)) -> Result<()> {
        let root = gio::File::for_uri(&self.root_uri);
        // On first access the location has to be mounted; the app does that beforehand.
        root.query_info("standard::type", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            .map_err(|e| gerr(&self.root_uri, e))?;
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err(DupeError::Cancelled);
            }
            let Ok(en) = dir.enumerate_children(
                ATTRS,
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                gio::Cancellable::NONE,
            ) else {
                continue;
            };
            while let Some(Ok(info)) = en.next_file(gio::Cancellable::NONE).transpose() {
                let name = info.name().to_string_lossy().into_owned();
                let child = dir.child(&name);
                match info.file_type() {
                    gio::FileType::Directory => {
                        if !skip_dir(&name) {
                            stack.push(child);
                        }
                    }
                    gio::FileType::Regular if !name.starts_with('.') => {
                        let inode = info.attribute_uint64("unix::inode");
                        let dev = info.attribute_uint32("unix::device");
                        on_entry(Entry {
                            uri: child.uri().to_string(),
                            size: info.size().max(0) as u64,
                            mtime: info.attribute_uint64("time::modified") as i64,
                            id: (inode != 0).then(|| format!("{dev}:{inode}")),
                            checksum: None,
                            dims: None,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn read_range(&self, e: &Entry, offset: u64, len: usize) -> Result<Vec<u8>> {
        let f = gio::File::for_uri(&e.uri);
        let stream = f.read(gio::Cancellable::NONE).map_err(|x| gerr(&e.uri, x))?;
        stream
            .seek(offset as i64, gio::glib::SeekType::Set, gio::Cancellable::NONE)
            .map_err(|x| gerr(&e.uri, x))?;
        let mut buf = vec![0u8; len];
        let (n, _) = stream
            .read_all(&mut buf, gio::Cancellable::NONE)
            .map_err(|x| gerr(&e.uri, x))?;
        buf.truncate(n);
        Ok(buf)
    }

    fn read_all(&self, e: &Entry) -> Result<Vec<u8>> {
        let (bytes, _) = gio::File::for_uri(&e.uri)
            .load_bytes(gio::Cancellable::NONE)
            .map_err(|x| gerr(&e.uri, x))?;
        Ok(bytes.to_vec())
    }

    fn trash(&self, e: &Entry) -> Result<()> {
        gio::File::for_uri(&e.uri)
            .trash(gio::Cancellable::NONE)
            .map_err(|x| gerr(&e.uri, x))
    }

    fn delete(&self, e: &Entry) -> Result<()> {
        gio::File::for_uri(&e.uri)
            .delete(gio::Cancellable::NONE)
            .map_err(|x| gerr(&e.uri, x))
    }

    fn local_path(&self, e: &Entry) -> Option<PathBuf> {
        e.uri.starts_with("file://").then(|| gio::File::for_uri(&e.uri).path()).flatten()
    }
}
