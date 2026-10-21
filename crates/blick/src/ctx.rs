//! Gemeinsamer Zustand aller Ansichten: Caches, Thumbnail-Lader, Immich-Client.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use blick_core::cache::{MemCache, ThumbStore};
use blick_core::{Pixels, Result as CoreResult};
use gtk::prelude::*;
use gtk::{gdk, glib};

/// Kantenlänge der Thumbnails in Pixeln. Die Kacheln sind kleiner,
/// so bleibt es auch bei Skalierung 2 scharf.
pub const THUMB_PX: u32 = 320;

/// RAM-Budget für dekodierte Vorschauen der Einzelansicht.
const PREVIEW_BUDGET: usize = 384 * 1024 * 1024;

pub struct Ctx {
    pub thumbs: ThumbLoader,
    pub previews: Arc<MemCache<(PathBuf, u32)>>,
    pub immich: RefCell<Option<blick_immich::Client>>,
}

impl Ctx {
    pub fn new() -> Self {
        let cache_dir = glib::user_cache_dir().join("blick").join("thumbs");
        Self {
            thumbs: ThumbLoader::new(Arc::new(ThumbStore::new(cache_dir))),
            previews: Arc::new(MemCache::new(PREVIEW_BUDGET)),
            immich: RefCell::new(None),
        }
    }

    pub fn immich(&self) -> Option<blick_immich::Client> {
        self.immich.borrow().clone()
    }
}

pub fn texture(px: Pixels) -> gdk::Texture {
    let stride = px.stride();
    let bytes = glib::Bytes::from_owned(px.data);
    gdk::MemoryTexture::new(
        px.width as i32,
        px.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
    .upcast()
}

struct Job {
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<CoreResult<Pixels>>,
}

type Queue = Arc<(Mutex<Vec<Job>>, Condvar)>;

/// Thumbnail-Erzeugung in eigenen Threads, neueste Anfrage zuerst.
///
/// Wer schnell durch einen großen Ordner scrollt, will die Kacheln sehen, bei
/// denen er gerade ist, nicht die, an denen er vor zwei Sekunden vorbeikam.
/// Deshalb ein Stapel statt einer Warteschlange, und Aufträge für Kacheln,
/// die nicht mehr sichtbar sind, fliegen vorher raus.
pub struct ThumbLoader {
    queue: Queue,
}

impl ThumbLoader {
    fn new(store: Arc<ThumbStore>) -> Self {
        let queue: Queue = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let workers = std::thread::available_parallelism()
            .map_or(2, |n| n.get().saturating_sub(1).clamp(1, 6));
        for i in 0..workers {
            let queue = queue.clone();
            let store = store.clone();
            std::thread::Builder::new()
                .name(format!("blick-thumb-{i}"))
                .spawn(move || worker(&queue, &store))
                .expect("Thread für Thumbnails ließ sich nicht starten");
        }
        Self { queue }
    }

    /// Liefert einen Empfänger für das Ergebnis und einen Schalter zum Abbrechen.
    /// Wird abgebrochen, schließt sich der Kanal ohne Ergebnis.
    pub fn request(
        &self,
        path: PathBuf,
    ) -> (async_channel::Receiver<CoreResult<Pixels>>, Arc<AtomicBool>) {
        let (tx, rx) = async_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().push(Job {
            path,
            cancel: cancel.clone(),
            tx,
        });
        cvar.notify_one();
        (rx, cancel)
    }
}

fn worker(queue: &Queue, store: &ThumbStore) {
    let (lock, cvar) = &**queue;
    loop {
        let job = {
            let mut jobs = lock.lock().unwrap();
            loop {
                jobs.retain(|j| !j.cancel.load(Ordering::Relaxed));
                if let Some(job) = jobs.pop() {
                    break job;
                }
                jobs = cvar.wait(jobs).unwrap();
            }
        };
        let result = store.get_or_create(&job.path, THUMB_PX);
        let _ = job.tx.send_blocking(result);
    }
}
