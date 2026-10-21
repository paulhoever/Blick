//! Seitenleiste: Zuschnitt, Stil, die drei Regler und die Speichern-Knöpfe.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use blick_core::Adjustments;
use blick_core::crop::{Aspect, Crop, PRESETS};
use blick_core::look::Look;
use gtk::glib;

pub struct EditPanel {
    pub widget: gtk::ScrolledWindow,
    pub save_copy: gtk::Button,
    pub remember: gtk::Button,
    pub overwrite: gtk::Button,
    pub choose_look: gtk::Button,
    pub import_looks: adw::ButtonRow,
    pub export_lut: adw::ButtonRow,
    /// „Zuschneiden …“, startet den Rahmen über dem Bild.
    pub crop_button: gtk::Button,
    crop_row: adw::ActionRow,
    aspect_row: adw::ComboRow,
    portrait_row: adw::SwitchRow,
    /// `None` = ganzes Bild.
    crop: std::cell::Cell<Option<Crop>>,
    /// Größe des Originals (gedreht), für die Pixelangabe.
    dims: std::cell::Cell<Option<(u32, u32)>>,
    on_aspect: RefCell<Vec<Rc<dyn Fn()>>>,
    scales: [gtk::Scale; 3],
    strength: gtk::Scale,
    strength_box: gtk::Box,
    look_name: adw::ActionRow,
    look: RefCell<Option<Arc<Look>>>,
    overwrite_allowed: std::cell::Cell<bool>,
    on_change: RefCell<Vec<Rc<dyn Fn()>>>,
}

const NAMES: [&str; 3] = ["Helligkeit", "Kontrast", "Sättigung"];

fn format_value(v: f64) -> String {
    match v.round() as i32 {
        0 => "0".into(),
        n if n > 0 => format!("+{n}"),
        // Echtes Minuszeichen, gleich breit wie das Plus.
        n => format!("\u{2212}{}", -n),
    }
}

impl EditPanel {
    pub fn new() -> Rc<Self> {
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();

        // Zuschnitt: Größe des Ausschnitts, Vorlage, Hoch/Quer.
        let crop_button = gtk::Button::builder()
            .icon_name("crop-symbolic")
            .tooltip_text("Zuschneiden (C)")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        crop_button.update_property(&[gtk::accessible::Property::Label("Zuschneiden")]);
        let crop_row = adw::ActionRow::builder()
            .title("Zuschnitt")
            .subtitle("Ganzes Bild")
            .subtitle_selectable(false)
            .css_classes(["property"])
            .activatable_widget(&crop_button)
            .build();
        crop_row.add_suffix(&crop_button);
        let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
        let aspect_row = adw::ComboRow::builder()
            .title("Seitenverhältnis")
            .model(&gtk::StringList::new(&names))
            .build();
        let portrait_row = adw::SwitchRow::builder()
            .title("Hochformat")
            .sensitive(false)
            .build();
        let crop_group = adw::PreferencesGroup::new();
        crop_group.add(&crop_row);
        crop_group.add(&aspect_row);
        crop_group.add(&portrait_row);
        column.append(&crop_group);

        // Stil: Zeile mit Namen und „Wählen …“, darunter die Stärke.
        let choose_look = gtk::Button::builder()
            .label("Wählen …")
            .tooltip_text("Stil wählen (S)")
            .valign(gtk::Align::Center)
            .build();
        let look_name = adw::ActionRow::builder()
            .title("Stil")
            .subtitle("Ohne")
            .subtitle_selectable(false)
            .css_classes(["property"])
            .build();
        look_name.add_suffix(&choose_look);
        let look_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        look_list.append(&look_name);
        let strength = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        strength.set_value(100.0);
        strength.set_draw_value(false);
        strength.update_property(&[gtk::accessible::Property::Label("Stärke des Stils")]);
        let strength_value = gtk::Label::new(Some("100"));
        strength_value.set_css_classes(&["numeric", "dim-label"]);
        let sw = strength_value.downgrade();
        strength.connect_value_changed(move |s| {
            if let Some(l) = sw.upgrade() {
                l.set_label(&format!("{:.0}", s.value()));
            }
        });
        let strength_box = slider_box("Stärke", &strength_value, &strength);
        strength_box.set_visible(false);
        let look_group = adw::PreferencesGroup::new();
        look_group.add(&look_list);
        look_group.add(&strength_box);
        column.append(&look_group);

        let adjust_group = adw::PreferencesGroup::builder()
            .title("Korrekturen")
            .build();
        let scales = NAMES.map(|name| {
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -100.0, 100.0, 1.0);
            scale.set_value(0.0);
            scale.set_draw_value(false);
            scale.add_mark(0.0, gtk::PositionType::Bottom, None);
            scale.update_property(&[gtk::accessible::Property::Label(name)]);
            let value = gtk::Label::new(Some("0"));
            value.set_css_classes(&["numeric", "dim-label"]);
            scale.connect_value_changed(glib_clone_value(&value));
            adjust_group.add(&slider_box(name, &value, &scale));
            scale
        });
        column.append(&adjust_group);

        let reset = gtk::Button::with_label("Zurücksetzen");
        let save_copy = gtk::Button::with_label("Als Kopie speichern");
        save_copy.set_css_classes(&["pill", "suggested-action"]);
        let remember = gtk::Button::with_label("Neben dem Bild merken");
        remember.set_tooltip_text(Some("Speichert Zuschnitt, Regler und Stil in einer .xmp-Datei daneben. Das Bild selbst bleibt unverändert."));
        let overwrite = gtk::Button::with_label("Original überschreiben");
        overwrite.add_css_class("destructive-action");

        let buttons = gtk::Box::new(gtk::Orientation::Vertical, 12);
        buttons.append(&save_copy);
        buttons.append(&remember);
        buttons.append(&overwrite);
        buttons.append(&reset);
        column.append(&buttons);

        let import_looks = adw::ButtonRow::builder()
            .title("Stile importieren …")
            .start_icon_name("document-open-symbolic")
            .tooltip_text(".cube, HaldCLUT-PNG oder Lightroom-Preset (.xmp), auch ganze Ordner")
            .build();
        let export_lut = adw::ButtonRow::builder()
            .title("Als LUT exportieren …")
            .start_icon_name("document-save-symbolic")
            .tooltip_text("Regler und Stil als .cube, für Resolve, Photoshop, darktable")
            .build();
        let extra = adw::PreferencesGroup::builder().title("Stile").build();
        extra.add(&import_looks);
        extra.add(&export_lut);
        column.append(&extra);

        let widget = gtk::ScrolledWindow::builder()
            .child(&column)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(280)
            .build();

        let this = Rc::new(Self {
            widget,
            save_copy,
            remember,
            overwrite,
            choose_look,
            import_looks,
            export_lut,
            crop_button,
            crop_row,
            aspect_row,
            portrait_row,
            crop: std::cell::Cell::new(None),
            dims: std::cell::Cell::new(None),
            on_aspect: RefCell::new(Vec::new()),
            scales,
            strength,
            strength_box,
            look_name,
            look: RefCell::new(None),
            overwrite_allowed: std::cell::Cell::new(true),
            on_change: RefCell::new(Vec::new()),
        });
        reset.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                this.set_look(None);
                this.reset();
            }
        ));
        this.aspect_row.connect_selected_notify(glib::clone!(
            #[weak]
            this,
            move |_| {
                let (a, _) = this.aspect();
                this.portrait_row.set_sensitive(a.has_orientation());
                this.notify_aspect();
            }
        ));
        this.portrait_row.connect_active_notify(glib::clone!(
            #[weak]
            this,
            move |_| this.notify_aspect()
        ));
        this.update_buttons();
        for s in this.scales.iter().chain([&this.strength]) {
            s.connect_value_changed(glib::clone!(
                #[weak]
                this,
                move |_| {
                    this.update_buttons();
                    this.notify();
                }
            ));
        }
        this
    }

    fn notify(&self) {
        let callbacks: Vec<_> = self.on_change.borrow().clone();
        for f in callbacks {
            f();
        }
    }

    fn notify_aspect(&self) {
        let callbacks: Vec<_> = self.on_aspect.borrow().clone();
        for f in callbacks {
            f();
        }
    }

    /// Vorlage oder Hoch/Quer geändert.
    pub fn connect_aspect_changed(&self, f: impl Fn() + 'static) {
        self.on_aspect.borrow_mut().push(Rc::new(f));
    }

    /// Gewählte Vorlage und ob hochkant.
    pub fn aspect(&self) -> (Aspect, bool) {
        let a = PRESETS
            .get(self.aspect_row.selected() as usize)
            .map_or(Aspect::Free, |(_, a)| *a);
        (a, self.portrait_row.is_active())
    }

    pub fn crop(&self) -> Option<Crop> {
        self.crop.get()
    }

    /// Ganzes Bild zählt als kein Zuschnitt.
    pub fn set_crop(&self, crop: Option<Crop>) {
        let crop = crop.filter(|c| !c.is_identity());
        if crop == self.crop.get() {
            return;
        }
        self.crop.set(crop);
        self.update_crop_label();
        self.update_buttons();
        self.notify();
    }

    /// Maße des Originals, sobald bekannt.
    pub fn set_dimensions(&self, dims: Option<(u32, u32)>) {
        self.dims.set(dims);
        self.update_crop_label();
    }

    fn update_crop_label(&self) {
        self.show_crop_size(self.crop.get());
    }

    /// Größenangabe für einen Rahmen, der noch nicht übernommen ist.
    pub fn show_crop_size(&self, crop: Option<Crop>) {
        let crop = crop.filter(|c| !c.is_identity());
        let text = match (crop, self.dims.get()) {
            (None, _) => "Ganzes Bild".to_owned(),
            (Some(c), Some((w, h))) => {
                let (cw, ch) = c.output_size(w, h);
                format!("{cw} × {ch} Pixel")
            }
            (Some(_), None) => "Zugeschnitten".to_owned(),
        };
        self.crop_row.set_subtitle(&text);
    }

    pub fn set_look(&self, look: Option<Arc<Look>>) {
        self.look_name
            .set_subtitle(look.as_ref().map_or("Ohne", |l| l.name.as_str()));
        self.strength_box.set_visible(look.is_some());
        self.look.replace(look);
        self.update_buttons();
        self.notify();
    }

    /// Werte aus einem Sidecar übernehmen.
    pub fn set_values(&self, adj: Adjustments, strength: f32) {
        self.scales[0].set_value(adj.brightness as f64 * 100.0);
        self.scales[1].set_value(adj.contrast as f64 * 100.0);
        self.scales[2].set_value(adj.saturation as f64 * 100.0);
        self.strength.set_value(strength as f64 * 100.0);
    }

    /// Gewählter Stil mit Stärke 0..=1.
    pub fn look(&self) -> Option<(Arc<Look>, f32)> {
        let l = self.look.borrow().clone()?;
        Some((l, (self.strength.value() / 100.0) as f32))
    }

    pub fn is_identity(&self) -> bool {
        self.colors_identity() && self.crop.get().is_none()
    }

    /// Nur Regler und Stil, ohne Zuschnitt (das, was eine LUT abbilden kann).
    pub fn colors_identity(&self) -> bool {
        self.adjustments().is_identity() && self.look.borrow().is_none()
    }

    pub fn adjustments(&self) -> Adjustments {
        let v = |i: usize| (self.scales[i].value() / 100.0) as f32;
        Adjustments {
            brightness: v(0),
            contrast: v(1),
            saturation: v(2),
        }
    }

    /// Stellt Regler und Zuschnitt zurück. Der Stil bleibt beim Blättern
    /// erhalten, so lassen sich mehrere Bilder im selben Look ansehen.
    pub fn reset(&self) {
        for s in &self.scales {
            s.set_value(0.0);
        }
        self.set_crop(None);
    }

    /// RAW, HEIC & Co. lassen sich nur als Kopie speichern.
    pub fn set_overwrite_allowed(&self, allowed: bool) {
        self.overwrite_allowed.set(allowed);
        self.overwrite.set_tooltip_text((!allowed).then_some(
            "Dieses Format kann Blick nur lesen, nicht schreiben. Als Kopie speichern geht.",
        ));
        self.update_buttons();
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.on_change.borrow_mut().push(Rc::new(f));
    }

    fn update_buttons(&self) {
        let changed = !self.is_identity();
        self.save_copy.set_sensitive(changed);
        self.overwrite
            .set_sensitive(changed && self.overwrite_allowed.get());
        self.export_lut.set_sensitive(!self.colors_identity());
    }
}

/// Regler wie in den Mockups der GNOME-Bildanzeige: Name und Wert über der Leiste.
fn slider_box(name: &str, value: &gtk::Label, scale: &gtk::Scale) -> gtk::Box {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    head.append(value);
    let group = gtk::Box::new(gtk::Orientation::Vertical, 2);
    group.set_margin_top(6);
    group.append(&head);
    group.append(scale);
    group
}

fn glib_clone_value(label: &gtk::Label) -> impl Fn(&gtk::Scale) + 'static {
    let label = label.downgrade();
    move |scale| {
        if let Some(label) = label.upgrade() {
            label.set_label(&format_value(scale.value()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_value;

    #[test]
    fn values_have_signs() {
        assert_eq!(format_value(0.2), "0");
        assert_eq!(format_value(12.0), "+12");
        assert_eq!(format_value(-7.0), "\u{2212}7");
    }
}
