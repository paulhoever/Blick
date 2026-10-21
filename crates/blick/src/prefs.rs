//! Einstellungen: Verbindung zu Immich.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config;
use crate::runtime;

const HELP: &str = "Blick lädt Bilder zu Immich hoch und zeigt deine Alben. \
Den API-Schlüssel legst du in Immich unter Kontoeinstellungen, API-Schlüssel an. \
Er braucht die Rechte user.read, album.read, asset.read, asset.view, asset.upload und albumAsset.create.";

pub fn present(
    parent: &impl IsA<gtk::Widget>,
    settings: &gio::Settings,
    on_connected: impl Fn(blick_immich::Client) + 'static,
) {
    let url = adw::EntryRow::builder()
        .title("Server-Adresse")
        .text(settings.string("immich-url"))
        .build();
    url.set_input_purpose(gtk::InputPurpose::Url);
    let key = adw::PasswordEntryRow::builder()
        .title("API-Schlüssel")
        .build();

    let check = gtk::Button::builder()
        .label("Prüfen und speichern")
        .valign(gtk::Align::Center)
        .build();
    let status = adw::ActionRow::builder()
        .title("Verbindung")
        .subtitle("Noch nicht geprüft")
        .build();
    status.add_suffix(&check);

    let group = adw::PreferencesGroup::builder()
        .title("Immich")
        .description(HELP)
        .build();
    group.add(&url);
    group.add(&key);
    group.add(&status);

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let settings = settings.clone();
    let on_connected = Rc::new(on_connected);
    check.connect_clicked(glib::clone!(
        #[weak]
        url,
        #[weak]
        key,
        #[weak]
        status,
        move |button| {
            let (server, secret) = (url.text().trim().to_owned(), key.text().trim().to_owned());
            if secret.is_empty() {
                status.set_subtitle("Bitte einen API-Schlüssel eintragen.");
                return;
            }
            let client = match blick_immich::Client::new(&server, &secret) {
                Ok(c) => c,
                Err(e) => {
                    status.set_subtitle(&e.to_string());
                    return;
                }
            };
            button.set_sensitive(false);
            status.set_subtitle("Prüfe …");
            let settings = settings.clone();
            let on_connected = on_connected.clone();
            glib::spawn_future_local(glib::clone!(
                #[weak]
                button,
                #[weak]
                status,
                async move {
                    let c = client.clone();
                    let me = runtime::spawn(async move { Ok(c.me().await?) }).await;
                    button.set_sensitive(true);
                    let user = match me {
                        Ok(u) => u,
                        Err(e) => {
                            status.set_subtitle(&format!("{e:#}"));
                            return;
                        }
                    };
                    let _ = settings.set_string("immich-url", &server);
                    let stored = runtime::spawn(config::store_api_key(server, secret)).await;
                    let mut text = format!("Angemeldet als {} ({})", user.name, user.email);
                    if let Err(e) = stored {
                        tracing::warn!("Schlüsselbund: {e:#}");
                        text.push_str(". Der Schlüsselbund ist nicht erreichbar, der Schlüssel gilt nur bis zum Beenden.");
                    }
                    status.set_subtitle(&text);
                    on_connected(client);
                }
            ));
        }
    ));
    dialog.present(Some(parent));
}
