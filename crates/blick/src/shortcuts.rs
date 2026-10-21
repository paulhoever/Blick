//! Tastenkürzel-Übersicht (Strg+?) als `AdwShortcutsDialog`.

const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Allgemein",
        &[
            ("Ordner öffnen", "<Control>o"),
            ("Dubletten suchen", "<Control><Shift>d"),
            ("Einstellungen", "<Control>comma"),
            ("Tastenkürzel", "<Control>question"),
            ("Hauptmenü", "F10"),
            ("Vollbild", "F11"),
            ("Fenster schließen", "<Control>w"),
            ("Beenden", "<Control>q"),
        ],
    ),
    (
        "Kontaktbogen",
        &[
            ("Bild unter dem Fokus groß zeigen", "space"),
            ("Alle auswählen", "<Control>a"),
            ("Nach links drehen", "<Control><Shift>r"),
            ("Nach rechts drehen", "<Control>r"),
            ("Zu Immich hochladen", "<Control>u"),
            ("In den Papierkorb", "Delete"),
            ("Bildinfos", "<Alt>Return"),
        ],
    ),
    (
        "Einzelansicht",
        &[
            ("Vorheriges Bild", "Left"),
            ("Nächstes Bild", "Right"),
            ("Vergrößern", "plus"),
            ("Verkleinern", "minus"),
            ("Einpassen", "0"),
            ("Originalgröße", "1"),
            ("Zurück zum Kontaktbogen", "space"),
            ("Bearbeiten", "e"),
            ("Stil wählen", "s"),
            ("Zuschneiden", "c"),
            ("Vollbild", "f"),
        ],
    ),
    (
        "Video",
        &[
            ("Abspielen/Anhalten", "space"),
            ("Ton an/aus", "m"),
            ("Lauter", "Up"),
            ("Leiser", "Down"),
        ],
    ),
    (
        "Zuschneiden",
        &[
            ("Rahmen verschieben", "Left Right Up Down"),
            (
                "Rahmen größer oder kleiner",
                "<Shift>Left <Shift>Right <Shift>Up <Shift>Down",
            ),
            ("Übernehmen", "Return"),
            ("Abbrechen", "Escape"),
        ],
    ),
];

pub fn dialog() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    for (title, items) in SECTIONS {
        let section = adw::ShortcutsSection::new(Some(title));
        for (name, accel) in *items {
            section.add(adw::ShortcutsItem::new(name, accel));
        }
        dialog.add(section);
    }
    dialog
}
