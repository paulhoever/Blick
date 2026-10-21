//! Hell/Dunkel-Umschalter wie in GNOME Text Editor: drei runde Felder oben im
//! Hauptmenü. Die Wahl liegt in GSettings und gilt für alle Fenster.

use adw::prelude::*;
use gtk::gio;

const SCHEMES: [(&str, &str, &str); 3] = [
    ("default", "follow", "Systemeinstellung folgen"),
    ("light", "light", "Hell"),
    ("dark", "dark", "Dunkel"),
];

pub fn apply(settings: &gio::Settings) {
    let scheme = match settings.string("color-scheme").as_str() {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// Das Widget für den Menüeintrag mit `custom="theme"`.
pub fn selector(settings: &gio::Settings) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(18)
        .halign(gtk::Align::Center)
        .build();
    row.add_css_class("themeselector");
    let current = settings.string("color-scheme");
    let mut group: Option<gtk::CheckButton> = None;
    for (key, class, label) in SCHEMES {
        let b = gtk::CheckButton::builder()
            .tooltip_text(label)
            .active(current == key)
            .build();
        b.add_css_class(class);
        b.update_property(&[gtk::accessible::Property::Label(label)]);
        if let Some(g) = &group {
            b.set_group(Some(g));
        } else {
            group = Some(b.clone());
        }
        let settings = settings.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                let _ = settings.set_string("color-scheme", key);
                apply(&settings);
            }
        });
        row.append(&b);
    }
    row
}
