//! Lokale Bilder zu Immich hochladen, optional gleich in ein Album.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use blick_immich::{Client, Error, UploadStatus};
use futures_util::{StreamExt, stream};

use crate::runtime;
use crate::sheet::{Sheet, bilder};

/// So viele Dateien gleichzeitig. Mehr füllt die Leitung nicht besser,
/// sondern lässt nur den Server parallel Vorschauen rechnen.
const PARALLEL: usize = 3;

#[derive(Default)]
struct Summary {
    created: Vec<String>,
    duplicates: Vec<String>,
    failed: Vec<(PathBuf, Error)>,
    cancelled: usize,
    album: Option<(String, Result<usize, Error>)>,
}

impl Summary {
    fn text(&self) -> String {
        if let Some((_, Error::Unauthorized)) = self.failed.first() {
            return "Immich hat den API-Schlüssel abgelehnt. Prüf ihn in den Einstellungen.".into();
        }
        let mut parts = vec![format!("{} hochgeladen", bilder(self.created.len()))];
        if !self.duplicates.is_empty() {
            parts.push(format!("{} waren schon da", self.duplicates.len()));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} fehlgeschlagen", self.failed.len()));
        }
        if self.cancelled > 0 {
            parts.push(format!("{} abgebrochen", self.cancelled));
        }
        let mut text = parts.join(", ");
        match &self.album {
            Some((name, Ok(_))) => text.push_str(&format!(". Im Album {name}.")),
            Some((name, Err(e))) => text.push_str(&format!(". Album {name}: {e}")),
            None => {}
        }
        text
    }
}

/// Fragt nach dem Album und lädt hoch. Fortschritt erscheint im Kontaktbogen.
pub async fn run(sheet: &Sheet, client: Client, paths: Vec<PathBuf>, toasts: &adw::ToastOverlay) {
    let albums = {
        let c = client.clone();
        runtime::spawn(async move { Ok(c.albums().await?) })
            .await
            .unwrap_or_default()
    };
    let names: Vec<&str> = std::iter::once("Kein Album")
        .chain(albums.iter().map(|a| a.album_name.as_str()))
        .collect();
    let dropdown = gtk::DropDown::from_strings(&names);
    dropdown.update_property(&[gtk::accessible::Property::Label("Album")]);

    let dialog = adw::AlertDialog::new(
        Some(&format!("{} zu Immich hochladen", bilder(paths.len()))),
        Some(
            "Bilder, die Immich schon hat, erkennt der Server an der Prüfsumme und legt sie nicht doppelt an.",
        ),
    );
    dialog.set_extra_child(Some(&dropdown));
    dialog.add_responses(&[("cancel", "Abbrechen"), ("upload", "Hochladen")]);
    dialog.set_response_appearance("upload", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("upload"));
    dialog.set_close_response("cancel");
    if dialog.choose_future(Some(&sheet.page)).await != "upload" {
        return;
    }
    let album = (dropdown.selected() as usize)
        .checked_sub(1)
        .and_then(|i| albums.get(i))
        .cloned();

    let cancel = Arc::new(AtomicBool::new(false));
    sheet.set_busy(cancel.clone());
    let total = paths.len();
    sheet.set_progress("Lade hoch:", 0, total);

    let (tx, rx) = async_channel::unbounded::<()>();
    let task = runtime::spawn_handle(async move {
        let results: Vec<_> = stream::iter(paths)
            .map(|path| {
                let (client, cancel, tx) = (client.clone(), cancel.clone(), tx.clone());
                async move {
                    if cancel.load(Ordering::Relaxed) {
                        return (path, None);
                    }
                    let r = client.upload(&path).await;
                    let _ = tx.send(()).await;
                    (path, Some(r))
                }
            })
            .buffer_unordered(PARALLEL)
            .collect()
            .await;
        drop(tx);

        let mut s = Summary::default();
        for (path, r) in results {
            match r {
                None => s.cancelled += 1,
                Some(Ok(resp)) if resp.status == UploadStatus::Duplicate => {
                    s.duplicates.push(resp.id)
                }
                Some(Ok(resp)) => s.created.push(resp.id),
                Some(Err(e)) => s.failed.push((path, e)),
            }
        }
        if let Some(album) = album {
            let ids: Vec<String> = s.created.iter().chain(&s.duplicates).cloned().collect();
            if !ids.is_empty() {
                let r = client
                    .add_to_album(&album.id, &ids)
                    .await
                    .map(|r| r.iter().filter(|x| x.success).count());
                s.album = Some((album.album_name, r));
            }
        }
        s
    });

    let mut done = 0;
    while rx.recv().await.is_ok() {
        done += 1;
        sheet.set_progress("Lade hoch:", done, total);
    }
    let text = match task.await {
        Ok(summary) => {
            for (path, e) in &summary.failed {
                tracing::warn!("Upload {}: {e}", path.display());
            }
            summary.text()
        }
        Err(e) => format!("Hochladen abgebrochen: {e}"),
    };
    sheet.set_idle();
    toasts.add_toast(adw::Toast::builder().title(text).timeout(6).build());
}
