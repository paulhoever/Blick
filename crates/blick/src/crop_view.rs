//! Cropping: the whole image, with a frame and handles on top.
//!
//! The frame is kept in image pixels of the preview and handed out as a [`Crop`]
//! (fractions). Dragging corners and edges resizes it, dragging inside
//! moves it. Keyboard: arrows move, with Shift they resize, Enter applies,
//! Esc cancels (see viewer).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use blick_core::crop::{Aspect, Crop, Handle, Rect};
use gtk::prelude::*;
use gtk::{gdk, glib};

/// Margin around the image so handles stay reachable at the image edge.
const MARGIN: f64 = 24.0;
/// More room at the bottom: the bar with Cancel and Crop floats there.
const BOTTOM: f64 = 72.0;
/// Handle hit radius in screen pixels.
const GRAB: f64 = 14.0;
/// Arrow key: step in fractions of the shorter image edge.
const KEY_STEP: f64 = 0.01;

type OnChange = Box<dyn Fn(Crop)>;

pub struct CropView {
    pub widget: gtk::Overlay,
    picture: gtk::Picture,
    area: gtk::DrawingArea,
    /// Size of the shown preview in pixels.
    img: Cell<(f64, f64)>,
    crop: Cell<Crop>,
    aspect: Cell<(Aspect, bool)>,
    /// Handle and frame at the moment of grabbing.
    drag: Cell<Option<(Handle, Rect)>>,
    on_change: RefCell<Option<OnChange>>,
}

impl CropView {
    pub fn new() -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .margin_top(MARGIN as i32)
            .margin_bottom(BOTTOM as i32)
            .margin_start(MARGIN as i32)
            .margin_end(MARGIN as i32)
            .build();
        let area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .accessible_role(gtk::AccessibleRole::Img)
            .build();
        area.update_property(&[
            gtk::accessible::Property::Label("Crop"),
            gtk::accessible::Property::Description(
                "Arrow keys move the frame, with Shift they change its size. Enter applies, Escape cancels.",
            ),
        ]);
        let widget = gtk::Overlay::builder()
            .child(&picture)
            .hexpand(true)
            .vexpand(true)
            .visible(false)
            .build();
        widget.add_overlay(&area);

        let this = Rc::new(Self {
            widget,
            picture,
            area,
            img: Cell::new((0.0, 0.0)),
            crop: Cell::new(Crop::FULL),
            aspect: Cell::new((Aspect::Free, false)),
            drag: Cell::new(None),
            on_change: RefCell::new(None),
        });

        let weak = Rc::downgrade(&this);
        this.area.set_draw_func(move |_, cr, _, _| {
            if let Some(this) = weak.upgrade() {
                this.draw(cr);
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        drag.connect_drag_begin(glib::clone!(
            #[weak]
            this,
            move |g, x, y| {
                this.area.grab_focus();
                match this.hit(x, y) {
                    Some(h) => this.drag.set(Some((h, this.rect()))),
                    None => {
                        g.set_state(gtk::EventSequenceState::Denied);
                    }
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak]
            this,
            move |_, dx, dy| {
                let Some((handle, start)) = this.drag.get() else {
                    return;
                };
                let s = this.scale();
                if s <= 0.0 {
                    return;
                }
                let (iw, ih) = this.img.get();
                let r = start.dragged(handle, dx / s, dy / s, this.ratio(), iw, ih);
                this.set_rect(r);
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak]
            this,
            move |_, _, _| this.drag.set(None)
        ));
        this.area.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak]
            this,
            move |_, x, y| {
                if this.drag.get().is_some() {
                    return;
                }
                let name = this.hit(x, y).map(cursor_name);
                this.area.set_cursor_from_name(name);
            }
        ));
        this.area.add_controller(motion);
        this
    }

    /// On every drag, for the size readout in the sidebar.
    pub fn connect_changed(&self, f: impl Fn(Crop) + 'static) {
        self.on_change.replace(Some(Box::new(f)));
    }

    /// Image the frame sits on (without crop).
    pub fn set_texture(&self, tex: Option<&gdk::Texture>) {
        self.picture.set_paintable(tex);
        if let Some(t) = tex {
            self.img.set((t.width() as f64, t.height() as f64));
        }
        self.area.queue_draw();
    }

    pub fn crop(&self) -> Crop {
        self.crop.get()
    }

    pub fn set_crop(&self, c: Crop) {
        self.crop.set(c);
        self.area.queue_draw();
    }

    /// New aspect ratio. With `fit`, the largest fitting frame right away
    /// around the previous center, otherwise it applies from the next drag.
    pub fn set_aspect(&self, aspect: Aspect, portrait: bool, fit: bool) {
        self.aspect.set((aspect, portrait));
        let (iw, ih) = self.img.get();
        if fit && iw > 0.0 && ih > 0.0 {
            let r = self.rect().fit_ratio(self.ratio(), iw, ih);
            self.set_rect(r);
        }
    }

    /// Back to the whole image, in the chosen ratio.
    pub fn reset(&self) {
        let (iw, ih) = self.img.get();
        if iw > 0.0 && ih > 0.0 {
            self.set_rect(Rect::full(iw, ih).fit_ratio(self.ratio(), iw, ih));
        }
    }

    pub fn grab_focus(&self) {
        self.area.grab_focus();
    }

    /// Arrow keys. `true` if the key did something.
    pub fn key(&self, key: gdk::Key, shift: bool) -> bool {
        let (dx, dy) = match key {
            gdk::Key::Left | gdk::Key::KP_Left => (-1.0, 0.0),
            gdk::Key::Right | gdk::Key::KP_Right => (1.0, 0.0),
            gdk::Key::Up | gdk::Key::KP_Up => (0.0, -1.0),
            gdk::Key::Down | gdk::Key::KP_Down => (0.0, 1.0),
            _ => return false,
        };
        let (iw, ih) = self.img.get();
        if iw <= 0.0 || ih <= 0.0 {
            return true;
        }
        let step = (iw.min(ih) * KEY_STEP).max(1.0);
        let start = self.rect();
        let r = if shift {
            start.dragged(Handle::SE, dx * step, dy * step, self.ratio(), iw, ih)
        } else {
            start.moved(dx * step, dy * step, iw, ih)
        };
        self.set_rect(r);
        true
    }

    fn ratio(&self) -> Option<f64> {
        let (a, portrait) = self.aspect.get();
        let (iw, ih) = self.img.get();
        a.ratio(iw, ih, portrait)
    }

    fn rect(&self) -> Rect {
        let (iw, ih) = self.img.get();
        self.crop.get().to_rect(iw, ih)
    }

    fn set_rect(&self, r: Rect) {
        let (iw, ih) = self.img.get();
        let c = Crop::from_rect(r, iw, ih);
        self.crop.set(c);
        self.area.queue_draw();
        if let Some(f) = self.on_change.borrow().as_ref() {
            f(c);
        }
    }

    /// Image pixel → screen: scale and top left corner of the image.
    fn placement(&self) -> (f64, f64, f64) {
        let (iw, ih) = self.img.get();
        let (w, h) = (self.area.width() as f64, self.area.height() as f64);
        let (bw, bh) = (w - 2.0 * MARGIN, h - MARGIN - BOTTOM);
        if iw <= 0.0 || ih <= 0.0 || bw <= 0.0 || bh <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        let s = (bw / iw).min(bh / ih);
        (
            s,
            MARGIN + (bw - iw * s) / 2.0,
            MARGIN + (bh - ih * s) / 2.0,
        )
    }

    fn scale(&self) -> f64 {
        self.placement().0
    }

    /// Rahmen in Bildschirmkoordinaten.
    fn screen_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let (s, ox, oy) = self.placement();
        if s <= 0.0 {
            return None;
        }
        let r = self.rect();
        Some((ox + r.x * s, oy + r.y * s, r.w * s, r.h * s))
    }

    /// Which handle is under the pointer. Corners before edges before the inside.
    fn hit(&self, x: f64, y: f64) -> Option<Handle> {
        let (rx, ry, rw, rh) = self.screen_rect()?;
        let near = |a: f64, b: f64| (a - b).abs() <= GRAB;
        let in_x = x >= rx - GRAB && x <= rx + rw + GRAB;
        let in_y = y >= ry - GRAB && y <= ry + rh + GRAB;
        let (l, r, t, b) = (near(x, rx), near(x, rx + rw), near(y, ry), near(y, ry + rh));
        Some(match (l, r, t, b) {
            (true, _, true, _) => Handle::NW,
            (_, true, true, _) => Handle::NE,
            (true, _, _, true) => Handle::SW,
            (_, true, _, true) => Handle::SE,
            (true, _, _, _) if in_y => Handle::W,
            (_, true, _, _) if in_y => Handle::E,
            (_, _, true, _) if in_x => Handle::N,
            (_, _, _, true) if in_x => Handle::S,
            _ if x > rx && x < rx + rw && y > ry && y < ry + rh => Handle::Move,
            _ => return None,
        })
    }

    fn draw(&self, cr: &gtk::cairo::Context) {
        let Some((rx, ry, rw, rh)) = self.screen_rect() else {
            return;
        };
        let (s, ox, oy) = self.placement();
        let (iw, ih) = self.img.get();
        // Darken outside the frame, only over the image.
        cr.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
        cr.rectangle(ox, oy, iw * s, ih * s);
        cr.rectangle(rx, ry, rw, rh);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        let _ = cr.fill();

        // Drittellinien.
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        for i in 1..3 {
            let f = i as f64 / 3.0;
            cr.move_to((rx + rw * f).round() + 0.5, ry);
            cr.line_to((rx + rw * f).round() + 0.5, ry + rh);
            cr.move_to(rx, (ry + rh * f).round() + 0.5);
            cr.line_to(rx + rw, (ry + rh * f).round() + 0.5);
        }
        let _ = cr.stroke();

        // Frame with a dark outline, so it stands out on bright images too.
        cr.rectangle(rx, ry, rw, rh);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
        cr.set_line_width(3.0);
        let _ = cr.stroke_preserve();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_line_width(1.5);
        let _ = cr.stroke();

        // Handles: circles at the corners, smaller ones at the edge midpoints.
        let dots = [
            (rx, ry, 7.0),
            (rx + rw, ry, 7.0),
            (rx, ry + rh, 7.0),
            (rx + rw, ry + rh, 7.0),
            (rx + rw / 2.0, ry, 5.0),
            (rx + rw / 2.0, ry + rh, 5.0),
            (rx, ry + rh / 2.0, 5.0),
            (rx + rw, ry + rh / 2.0, 5.0),
        ];
        for (x, y, r) in dots {
            cr.new_sub_path();
            cr.arc(x, y, r, 0.0, std::f64::consts::TAU);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let _ = cr.fill_preserve();
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }
    }
}

fn cursor_name(h: Handle) -> &'static str {
    match h {
        Handle::Move => "move",
        Handle::N | Handle::S => "ns-resize",
        Handle::E | Handle::W => "ew-resize",
        Handle::NW | Handle::SE => "nwse-resize",
        Handle::NE | Handle::SW => "nesw-resize",
    }
}
