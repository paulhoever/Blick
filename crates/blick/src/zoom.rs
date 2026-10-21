//! Bildfläche mit Zoom und Verschieben.
//!
//! Eingepasst füllt das Bild die Fläche. Gezoomt bekommt es eine feste Größe in
//! einem ScrolledWindow. Zoomstufe 1.0 heißt: ein Bildpixel auf ein
//! Gerätepixel. Sobald über die Einpassung hinaus gezoomt wird, fordert die
//! Fläche einmal das Bild in voller Auflösung an (`connect_need_full`), bis
//! dahin wird die Vorschau hochskaliert.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

const MAX_ZOOM: f64 = 8.0;
const STEP: f64 = 1.25;

type OnZoom = Box<dyn Fn(bool)>;

pub struct ZoomView {
    pub widget: gtk::ScrolledWindow,
    picture: gtk::Picture,
    /// `None` = eingepasst.
    zoom: Cell<Option<f64>>,
    /// Größe des Originals in Bildpixeln, nach Drehung.
    natural: Cell<(f64, f64)>,
    pointer: Cell<(f64, f64)>,
    /// Zeiger steht gerade über der Fläche (nicht über den Zoomknöpfen).
    hover: Cell<bool>,
    pinch_start: Cell<f64>,
    drag_start: Cell<(f64, f64)>,
    full_requested: Cell<bool>,
    /// Bild für die eingepasste Ansicht (Bildschirmgröße) und, falls geladen,
    /// das Original für den Zoom. Eingepasst wird nie das Original gezeichnet.
    fit_tex: RefCell<Option<gdk::Texture>>,
    full_tex: RefCell<Option<gdk::Texture>>,
    need_full: RefCell<Option<Box<dyn Fn()>>>,
    zoom_changed: RefCell<Option<OnZoom>>,
}

impl ZoomView {
    pub fn new() -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .build();
        let widget = gtk::ScrolledWindow::builder()
            .child(&picture)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .build();
        let this = Rc::new(Self {
            widget,
            picture,
            zoom: Cell::new(None),
            natural: Cell::new((0.0, 0.0)),
            pointer: Cell::new((0.0, 0.0)),
            hover: Cell::new(false),
            pinch_start: Cell::new(1.0),
            drag_start: Cell::new((0.0, 0.0)),
            full_requested: Cell::new(false),
            fit_tex: RefCell::new(None),
            full_tex: RefCell::new(None),
            need_full: RefCell::new(None),
            zoom_changed: RefCell::new(None),
        });
        this.setup_input();
        this
    }

    pub fn connect_need_full(&self, f: impl Fn() + 'static) {
        self.need_full.replace(Some(Box::new(f)));
    }

    /// `true`, wenn gezoomt ist. Die Einzelansicht blendet dann die Pfeile aus.
    pub fn connect_zoom_changed(&self, f: impl Fn(bool) + 'static) {
        self.zoom_changed.replace(Some(Box::new(f)));
    }

    pub fn picture(&self) -> &gtk::Picture {
        &self.picture
    }

    /// Neues Bild: zurück auf Einpassen.
    pub fn show(&self, paintable: Option<&gdk::Texture>, natural: Option<(u32, u32)>) {
        self.fit_tex.replace(paintable.cloned());
        self.full_tex.replace(None);
        self.picture.set_paintable(paintable);
        let nat = natural
            .or_else(|| paintable.map(|t| (t.width() as u32, t.height() as u32)))
            .unwrap_or((0, 0));
        self.natural.set((nat.0 as f64, nat.1 as f64));
        self.full_requested.set(false);
        self.fit();
    }

    /// Nur das Bild tauschen (Bearbeitung), Größe und Zoom bleiben.
    /// Die bearbeitete Fassung gibt es nur in Bildschirmgröße; sie ersetzt
    /// auch beim Zoomen das Original, damit man sieht, was man bearbeitet.
    pub fn replace(&self, paintable: Option<&gdk::Texture>) {
        self.fit_tex.replace(paintable.cloned());
        self.full_tex.replace(None);
        self.picture.set_paintable(paintable);
    }

    fn current_texture(&self) -> Option<gdk::Texture> {
        if self.is_zoomed() {
            self.full_tex
                .borrow()
                .clone()
                .or_else(|| self.fit_tex.borrow().clone())
        } else {
            self.fit_tex.borrow().clone()
        }
    }

    /// Originalmaße, sobald bekannt. Die Zoomstufe bezieht sich darauf.
    pub fn set_natural(&self, w: u32, h: u32) {
        if !self.is_zoomed() {
            self.natural.set((w as f64, h as f64));
        }
    }

    /// Schärferes Bild desselben Motivs (Vorschau, dann volle Auflösung).
    /// Die Bezugsgröße wächst nur: eine Vorschau ist kleiner als das Original
    /// und darf die Zoomstufe nicht verfälschen.
    pub fn upgrade(&self, texture: &gdk::Texture) {
        let (ow, _) = self.natural.get();
        let (nw, nh) = (texture.width() as f64, texture.height() as f64);
        if nw > ow {
            if let Some(z) = self.zoom.get()
                && ow > 0.0
            {
                self.zoom.set(Some(z * ow / nw));
            }
            self.natural.set((nw, nh));
        }
        // Größer als die Bildfläche: Original für den Zoom. Sonst Vorschau.
        let screen = self.widget.width().max(self.widget.height()) as f64 * self.scale_factor();
        if nw.max(nh) > screen * 1.5 {
            self.full_tex.replace(Some(texture.clone()));
        } else {
            self.fit_tex.replace(Some(texture.clone()));
        }
        self.picture.set_paintable(self.current_texture().as_ref());
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoom.get().is_some()
    }

    fn scale_factor(&self) -> f64 {
        self.widget.scale_factor().max(1) as f64
    }

    /// Zoomstufe, bei der das Bild gerade in die Fläche passt.
    fn fit_zoom(&self) -> f64 {
        let (w, h) = self.natural.get();
        let (vw, vh) = (self.widget.width() as f64, self.widget.height() as f64);
        if w <= 0.0 || h <= 0.0 || vw <= 0.0 || vh <= 0.0 {
            return 1.0;
        }
        let sf = self.scale_factor();
        (vw * sf / w).min(vh * sf / h).min(1.0)
    }

    pub fn fit(&self) {
        self.zoom.set(None);
        self.widget.hadjustment().set_value(0.0);
        self.widget.vadjustment().set_value(0.0);
        self.picture.set_size_request(-1, -1);
        self.picture.set_content_fit(gtk::ContentFit::Contain);
        self.picture.set_paintable(self.current_texture().as_ref());
        self.widget
            .set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        if let Some(f) = self.zoom_changed.borrow().as_ref() {
            f(false);
        }
    }

    fn current(&self) -> f64 {
        self.zoom.get().unwrap_or_else(|| self.fit_zoom())
    }

    /// Zoomanker für Tasten und Knöpfe: der Zeiger, solange er über dem Bild
    /// steht, sonst die Mitte.
    fn anchor(&self) -> (f64, f64) {
        if self.hover.get() {
            self.pointer.get()
        } else {
            (self.widget.width() as f64 / 2.0, self.widget.height() as f64 / 2.0)
        }
    }

    pub fn zoom_in(&self) {
        self.set_zoom(self.current() * STEP, self.anchor());
    }

    pub fn zoom_out(&self) {
        self.set_zoom(self.current() / STEP, self.anchor());
    }

    pub fn actual_size(&self) {
        self.set_zoom(1.0, self.anchor());
    }

    /// Zoomt so, dass der Bildpunkt unter `anchor` (Koordinaten der Fläche) stehen bleibt.
    pub fn set_zoom(&self, z: f64, anchor: (f64, f64)) {
        let fit = self.fit_zoom();
        let (nw, nh) = self.natural.get();
        if z <= fit * 1.001 || nw <= 0.0 {
            self.fit();
            return;
        }
        let z = z.min(MAX_ZOOM);
        let sf = self.scale_factor();
        let (hadj, vadj) = (self.widget.hadjustment(), self.widget.vadjustment());

        // Relative Lage des Ankers im Bild, vor der Änderung.
        let old = self.current();
        let (old_w, old_h) = (nw * old / sf, nh * old / sf);
        let (vw, vh) = (self.widget.width() as f64, self.widget.height() as f64);
        let (ox, oy) = if self.is_zoomed() {
            (hadj.value(), vadj.value())
        } else {
            // Eingepasst ist das Bild zentriert.
            (-(vw - old_w) / 2.0, -(vh - old_h) / 2.0)
        };
        let rx = ((ox + anchor.0) / old_w).clamp(0.0, 1.0);
        let ry = ((oy + anchor.1) / old_h).clamp(0.0, 1.0);

        let (new_w, new_h) = (nw * z / sf, nh * z / sf);
        self.zoom.set(Some(z));
        self.widget
            .set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        self.picture.set_content_fit(gtk::ContentFit::Fill);
        self.picture
            .set_size_request(new_w.round() as i32, new_h.round() as i32);

        // Die neuen Grenzen der Scrollbalken stehen erst nach dem Layout fest.
        let (ax, ay) = anchor;
        // Ein Tick-Callback läuft vor dem Layout, also so lange warten, bis die
        // Grenzen gewachsen sind (höchstens ein paar Frames).
        let tries = Cell::new(0);
        self.widget.add_tick_callback(move |_, _| {
            tries.set(tries.get() + 1);
            let grown = |adj: &gtk::Adjustment, size: f64| {
                adj.upper() + 1.0 >= size || size <= adj.page_size()
            };
            let ready = grown(&hadj, new_w) && grown(&vadj, new_h);
            if !ready && tries.get() < 10 {
                return glib::ControlFlow::Continue;
            }
            hadj.set_value(rx * new_w - ax);
            vadj.set_value(ry * new_h - ay);
            glib::ControlFlow::Break
        });

        if !self.full_requested.replace(true)
            && let Some(f) = self.need_full.borrow().as_ref()
        {
            f();
        }
        if let Some(f) = self.zoom_changed.borrow().as_ref() {
            f(true);
        }
    }

    fn setup_input(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, x, y| {
                this.pointer.set((x, y));
                this.hover.set(true);
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.hover.set(false)
        ));
        self.widget.add_controller(motion);

        // Strg + Mausrad zoomt, das Rad allein scrollt bzw. blättert (Einzelansicht).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |c, _, dy| {
                if !c
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                let f = if dy < 0.0 { STEP } else { 1.0 / STEP };
                this.set_zoom(this.current() * f, this.pointer.get());
                glib::Propagation::Stop
            }
        ));
        self.widget.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        pinch.connect_begin(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.pinch_start.set(this.current())
        ));
        pinch.connect_scale_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |g, scale| {
                let anchor = g.bounding_box_center().unwrap_or(this.pointer.get());
                this.set_zoom(this.pinch_start.get() * scale, anchor);
            }
        ));
        self.widget.add_controller(pinch);

        // Doppelklick: zwischen Einpassen und 100 % wechseln, am Klickpunkt.
        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, n, x, y| {
                if n == 2 {
                    if this.is_zoomed() {
                        this.fit();
                    } else {
                        this.set_zoom(1.0_f64.max(this.fit_zoom() * 2.0), (x, y));
                    }
                }
            }
        ));
        self.widget.add_controller(click);

        // Ziehen verschiebt, wenn gezoomt.
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _| {
                this.drag_start.set((
                    this.widget.hadjustment().value(),
                    this.widget.vadjustment().value(),
                ));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, dx, dy| {
                if this.is_zoomed() {
                    let (x0, y0) = this.drag_start.get();
                    this.widget.hadjustment().set_value(x0 - dx);
                    this.widget.vadjustment().set_value(y0 - dy);
                }
            }
        ));
        self.widget.add_controller(drag);
    }
}
