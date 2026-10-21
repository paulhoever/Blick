//! Upload local pictures to Immich, optionally straight into an album.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use blick_immich::{Client, Error, UploadStatus};
use futures_util::{StreamExt, stream};

use crate::runtime;
use crate::sheet::{Sheet, picture_count};

/// This many files at once. More doesn't fill the line any better,
/// it only makes the server render previews in parallel.
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
            return "Immich rejected the API key. Check it in the preferences.".into();
        }
        let mut parts = vec![format!("{} hochgeladen", picture_count(self.created.len()))];
        if !self.duplicates.is_empty() {
            parts.push(format!("{} were already there", self.duplicates.len()));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        if self.cancelled > 0 {
            parts.push(format!("{} cancelled", self.cancelled));
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

/// Asks for the album and uploads. Progress shows in the contact sheet.
pub async fn run(sheet: &Sheet, client: Client, paths: Vec<PathBuf>, toasts: &adw::ToastOverlay) {
    let albums = {
        let c = client.clone();
        runtime::spawn(async move { Ok(c.albums().await?) })
            .await
            .unwrap_or_default()
    };
    let names: Vec<&str> = std::iter::once("No album")
        .chain(albums.iter().map(|a| a.album_name.as_str()))
        .collect();
    let dropdown = gtk::DropDown::from_strings(&names);
    dropdown.update_property(&[gtk::accessible::Property::Label("Album")]);

    let dialog = adw::AlertDialog::new(
        Some(&format!("Upload {} to Immich", picture_count(paths.len()))),
        Some(
            "Pictures Immich already has are recognized by checksum on the server and not created twice.",
        ),
    );
    dialog.set_extra_child(Some(&dropdown));
    dialog.add_responses(&[("cancel", "Cancel"), ("upload", "Upload")]);
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
    sheet.set_progress("Uploading:", 0, total);

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
        sheet.set_progress("Uploading:", done, total);
    }
    let text = match task.await {
        Ok(summary) => {
            for (path, e) in &summary.failed {
                tracing::warn!("upload {}: {e}", path.display());
            }
            summary.text()
        }
        Err(e) => format!("Upload cancelled: {e}"),
    };
    sheet.set_idle();
    toasts.add_toast(adw::Toast::builder().title(text).timeout(6).build());
}
