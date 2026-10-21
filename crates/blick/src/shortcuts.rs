//! Keyboard shortcut overview (Ctrl+?) as an `AdwShortcutsDialog`.

const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "General",
        &[
            ("Open folder", "<Control>o"),
            ("Find duplicates", "<Control><Shift>d"),
            ("Preferences", "<Control>comma"),
            ("Keyboard shortcuts", "<Control>question"),
            ("Main menu", "F10"),
            ("Fullscreen", "F11"),
            ("Close window", "<Control>w"),
            ("Quit", "<Control>q"),
        ],
    ),
    (
        "Contact sheet",
        &[
            ("Show the focused picture large", "space"),
            ("Select all", "<Control>a"),
            ("Rotate left", "<Control><Shift>r"),
            ("Rotate right", "<Control>r"),
            ("Upload to Immich", "<Control>u"),
            ("Move to trash", "Delete"),
            ("Picture info", "<Alt>Return"),
        ],
    ),
    (
        "Single view",
        &[
            ("Previous picture", "Left"),
            ("Next picture", "Right"),
            ("Zoom in", "plus"),
            ("Zoom out", "minus"),
            ("Fit to window", "0"),
            ("Actual size", "1"),
            ("Back to the contact sheet", "space"),
            ("Edit", "e"),
            ("Choose look", "s"),
            ("Crop", "c"),
            ("Fullscreen", "f"),
        ],
    ),
    (
        "Video",
        &[
            ("Play/Pause", "space"),
            ("Mute/Unmute", "m"),
            ("Louder", "Up"),
            ("Quieter", "Down"),
        ],
    ),
    (
        "Crop",
        &[
            ("Move frame", "Left Right Up Down"),
            (
                "Make frame larger or smaller",
                "<Shift>Left <Shift>Right <Shift>Up <Shift>Down",
            ),
            ("Apply", "Return"),
            ("Cancel", "Escape"),
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
