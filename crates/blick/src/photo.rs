//! Ein Bild im Kontaktbogen, lokal oder aus Immich.
//!
//! Das Thumbnail hängt als Eigenschaft `texture` am Objekt. Kacheln binden
//! sich daran; ist es geladen, aktualisieren sie sich von selbst.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use base64::Engine;
use blick_immich::{RemoteAsset, ThumbSize};
use gtk::gdk;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use crate::ctx::{Ctx, texture};
use crate::runtime;

#[derive(Debug, Clone)]
pub enum Source {
    Local(PathBuf),
    Remote {
        id: String,
        thumbhash: Option<String>,
        video: bool,
    },
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Photo)]
    pub struct Photo {
        #[property(get, set, nullable)]
        pub texture: RefCell<Option<gdk::Texture>>,
        #[property(get)]
        pub title: RefCell<String>,
        /// Laufende Nummer im Bogen, beginnt bei 1.
        #[property(get)]
        pub number: Cell<u32>,
        #[property(get, set)]
        pub broken: Cell<bool>,
        pub source: OnceCell<Source>,
        /// Echtes Thumbnail da (nicht nur der Thumbhash-Platzhalter).
        pub loaded: Cell<bool>,
        pub loading: Cell<bool>,
        pub cancel: RefCell<Option<Box<dyn FnOnce()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Photo {
        const NAME: &'static str = "BlickPhoto";
        type Type = super::Photo;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Photo {}
}

glib::wrapper! {
    pub struct Photo(ObjectSubclass<imp::Photo>);
}

impl Photo {
    fn with_source(source: Source, title: String, number: u32) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.title.replace(title);
        imp.number.set(number);
        imp.source
            .set(source)
            .expect("Quelle wird genau einmal gesetzt");
        obj
    }

    pub fn local(path: PathBuf, number: u32) -> Self {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::with_source(Source::Local(path), title, number)
    }

    pub fn remote(asset: RemoteAsset, number: u32) -> Self {
        let title = asset
            .file_created_at
            .get(..10)
            .unwrap_or_default()
            .to_owned();
        Self::with_source(
            Source::Remote {
                id: asset.id,
                thumbhash: asset.thumbhash,
                video: !asset.is_image,
            },
            title,
            number,
        )
    }

    pub fn set_number(&self, n: u32) {
        self.imp().number.set(n);
    }

    pub fn source(&self) -> &Source {
        self.imp().source.get().expect("Quelle ist immer gesetzt")
    }

    pub fn path(&self) -> Option<&Path> {
        match self.source() {
            Source::Local(p) => Some(p),
            Source::Remote { .. } => None,
        }
    }

    pub fn is_video(&self) -> bool {
        match self.source() {
            Source::Local(p) => blick_core::formats::is_video(p),
            Source::Remote { video, .. } => *video,
        }
    }

    pub fn remote_id(&self) -> Option<&str> {
        match self.source() {
            Source::Remote { id, .. } => Some(id),
            Source::Local(_) => None,
        }
    }

    /// Startet das Laden des Thumbnails, falls nötig. Mehrfachaufrufe sind harmlos.
    pub fn ensure_thumbnail(&self, ctx: &Ctx) {
        let imp = self.imp();
        if imp.loaded.get() || imp.loading.get() {
            return;
        }
        imp.loading.set(true);
        match self.source().clone() {
            Source::Local(path) => self.load_local(ctx, path),
            Source::Remote { id, thumbhash, .. } => {
                if self.texture().is_none()
                    && let Some(placeholder) = thumbhash.as_deref().and_then(thumbhash_texture)
                {
                    self.set_texture(Some(placeholder));
                }
                self.load_remote(ctx, id);
            }
        }
    }

    fn load_local(&self, ctx: &Ctx, path: PathBuf) {
        let (rx, cancel) = ctx.thumbs.request(path);
        self.imp().cancel.replace(Some(Box::new(move || {
            cancel.store(true, Ordering::Relaxed)
        })));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = photo)]
            self,
            async move {
                // Ein geschlossener Kanal heißt: abgebrochen, später neu versuchen.
                let result = rx.recv().await;
                photo.finish_loading(
                    result
                        .ok()
                        .map(|r| r.map(texture).map_err(anyhow::Error::from)),
                );
            }
        ));
    }

    fn load_remote(&self, ctx: &Ctx, id: String) {
        let Some(client) = ctx.immich() else {
            self.imp().loading.set(false);
            return;
        };
        let handle = runtime::spawn_handle(async move {
            let bytes = client.thumbnail(&id, ThumbSize::Thumbnail).await?;
            let px = tokio::task::spawn_blocking(move || blick_core::decode::load_bytes(&bytes))
                .await??;
            anyhow::Ok(px)
        });
        let abort = handle.abort_handle();
        self.imp()
            .cancel
            .replace(Some(Box::new(move || abort.abort())));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = photo)]
            self,
            async move {
                let result = match handle.await {
                    Ok(r) => Some(r.map(texture)),
                    Err(e) if e.is_cancelled() => None,
                    Err(e) => Some(Err(e.into())),
                };
                photo.finish_loading(result);
            }
        ));
    }

    fn finish_loading(&self, result: Option<anyhow::Result<gdk::Texture>>) {
        let imp = self.imp();
        imp.loading.set(false);
        imp.cancel.take();
        match result {
            None => {}
            Some(Ok(tex)) => {
                imp.loaded.set(true);
                self.set_broken(false);
                self.set_texture(Some(tex));
            }
            Some(Err(e)) => {
                tracing::info!("Thumbnail für {}: {e:#}", self.title());
                imp.loaded.set(true);
                self.set_broken(true);
            }
        }
    }

    /// Kachel ist nicht mehr sichtbar: offenen Auftrag verwerfen.
    pub fn cancel_thumbnail(&self) {
        if let Some(cancel) = self.imp().cancel.take() {
            cancel();
        }
    }

    /// Nach Änderungen an der Datei: altes Thumbnail behalten, bis das neue da ist.
    pub fn reload(&self, ctx: &Ctx) {
        self.cancel_thumbnail();
        let imp = self.imp();
        imp.loaded.set(false);
        imp.loading.set(false);
        self.ensure_thumbnail(ctx);
    }
}

/// Immich liefert pro Bild einen Thumbhash (ca. 25 Byte). Daraus wird sofort
/// ein unscharfer Platzhalter, noch bevor das Thumbnail übers Netz kommt.
fn thumbhash_texture(hash: &str) -> Option<gdk::Texture> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(hash)
        .ok()?;
    let (w, h, rgba) = thumbhash::thumb_hash_to_rgba(&raw).ok()?;
    Some(texture(blick_core::Pixels {
        width: w as u32,
        height: h as u32,
        data: rgba,
    }))
}
