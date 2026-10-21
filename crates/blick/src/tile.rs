//! A tile in the contact sheet: square print, with picture number and name below.

use std::cell::RefCell;

use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::photo::Photo;

pub const TILE_PX: i32 = 168;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Tile {
        pub picture: gtk::Picture,
        pub broken: gtk::Image,
        /// Marker for videos, bottom left of the print.
        pub video: gtk::Image,
        pub video_badge: gtk::Box,
        pub number: gtk::Label,
        /// Running number of the bound picture, for the space bar in the sheet.
        pub position: std::cell::Cell<u32>,
        pub name: gtk::Label,
        pub bindings: RefCell<Vec<glib::Binding>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tile {
        const NAME: &'static str = "BlickTile";
        type Type = super::Tile;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for Tile {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_spacing(6);
            // Air between the prints without custom CSS.
            obj.set_margin_start(6);
            obj.set_margin_end(6);
            obj.set_margin_top(6);
            obj.set_margin_bottom(6);

            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_size_request(TILE_PX, TILE_PX);
            self.picture.set_can_shrink(true);

            self.broken.set_icon_name(Some("image-missing-symbolic"));
            self.broken.set_pixel_size(32);
            self.broken.add_css_class("dim-label");
            self.broken.set_visible(false);

            // Rounded print as in the GNOME Photos mockups: `card`
            // supplies radius and base tone, `overflow` clips the image.
            let frame = gtk::Overlay::new();
            frame.add_css_class("card");
            frame.set_overflow(gtk::Overflow::Hidden);
            frame.set_child(Some(&self.picture));
            frame.add_overlay(&self.broken);

            // Badge like the floating bars of the single view (`toolbar` + `osd`).
            self.video
                .set_icon_name(Some("media-playback-start-symbolic"));
            self.video_badge.set_css_classes(&["toolbar", "osd"]);
            self.video_badge.append(&self.video);
            self.video_badge.set_halign(gtk::Align::Start);
            self.video_badge.set_valign(gtk::Align::End);
            self.video_badge.set_margin_start(6);
            self.video_badge.set_margin_bottom(6);
            self.video_badge.set_can_target(false);
            self.video_badge.set_visible(false);
            frame.add_overlay(&self.video_badge);
            obj.append(&frame);

            self.number
                .set_css_classes(&["caption", "numeric", "dim-label"]);
            self.number.set_xalign(0.0);
            self.name.add_css_class("caption");
            self.name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            self.name.set_hexpand(true);
            self.name.set_xalign(0.0);
            self.name.set_width_chars(1);

            let caption = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            caption.set_margin_start(2);
            caption.set_margin_end(2);
            caption.append(&self.number);
            caption.append(&self.name);
            obj.append(&caption);
        }
    }

    impl WidgetImpl for Tile {}
    impl BoxImpl for Tile {}
}

glib::wrapper! {
    pub struct Tile(ObjectSubclass<imp::Tile>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for Tile {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Tile {
    pub fn bind(&self, photo: &Photo) {
        let imp = self.imp();
        imp.number.set_label(&format!("{:>3}", photo.number()));
        imp.position.set(photo.number().saturating_sub(1));
        imp.name.set_label(&photo.title());
        imp.video_badge.set_visible(photo.is_video());
        let label = if photo.is_video() {
            format!("Video {}", photo.title())
        } else {
            photo.title()
        };
        self.update_property(&[gtk::accessible::Property::Label(&label)]);
        let mut bindings = imp.bindings.borrow_mut();
        bindings.push(
            photo
                .bind_property("texture", &imp.picture, "paintable")
                .sync_create()
                .build(),
        );
        bindings.push(
            photo
                .bind_property("broken", &imp.broken, "visible")
                .sync_create()
                .build(),
        );
    }

    /// Position in the sheet (0-based).
    pub fn position(&self) -> u32 {
        self.imp().position.get()
    }

    pub fn unbind(&self) {
        for b in self.imp().bindings.take() {
            b.unbind();
        }
        self.imp()
            .picture
            .set_paintable(None::<&gtk::gdk::Paintable>);
    }
}
