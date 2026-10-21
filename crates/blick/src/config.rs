//! Einstellungen (GSettings) und der Immich-Schlüssel (Schlüsselbund).

use std::collections::HashMap;

use anyhow::Context;
use gtk::gio;

pub const APP_ID: &str = "io.github.paulhoever.Blick";

const KEYRING_LABEL: &str = "Blick: Immich-API-Schlüssel";

pub fn settings() -> gio::Settings {
    let default = gio::SettingsSchemaSource::default();
    if let Some(schema) = default.as_ref().and_then(|s| s.lookup(APP_ID, true)) {
        return gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    }
    // Nicht installiert, also `cargo run`: das Schema aus dem Build nehmen.
    let dir = concat!(env!("OUT_DIR"), "/schemas");
    let source = gio::SettingsSchemaSource::from_directory(dir, default.as_ref(), false)
        .expect("GSettings-Schema nicht gefunden");
    let schema = source
        .lookup(APP_ID, false)
        .expect("Schema io.github.paulhoever.Blick fehlt");
    gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
}

fn attributes(url: &str) -> HashMap<&str, &str> {
    HashMap::from([("application", APP_ID), ("server", url)])
}

/// Zum Entwickeln und Testen ohne Schlüsselbund: `BLICK_IMMICH_URL` und
/// `BLICK_IMMICH_KEY` haben Vorrang vor den gespeicherten Werten.
pub fn env_override() -> Option<(String, String)> {
    Some((
        std::env::var("BLICK_IMMICH_URL").ok()?,
        std::env::var("BLICK_IMMICH_KEY").ok()?,
    ))
}

/// Läuft auf der tokio-Runtime, oo7 braucht sie.
pub async fn load_api_key(url: String) -> anyhow::Result<Option<String>> {
    let keyring = oo7::Keyring::new()
        .await
        .context("Schlüsselbund nicht erreichbar")?;
    let items = keyring.search_items(&attributes(&url)).await?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let secret = item.secret().await?;
    Ok(Some(String::from_utf8(secret.to_vec())?))
}

pub async fn store_api_key(url: String, key: String) -> anyhow::Result<()> {
    let keyring = oo7::Keyring::new()
        .await
        .context("Schlüsselbund nicht erreichbar")?;
    keyring
        .create_item(KEYRING_LABEL, &attributes(&url), key.as_bytes(), true)
        .await?;
    Ok(())
}
