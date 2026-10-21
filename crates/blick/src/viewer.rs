//! Single view: one picture on black, paging, editing, cropping.
//!
//! On open the thumbnail from the contact sheet is there right away and gets
//! replaced as soon as the screen-size preview is ready. The neighbors
//! are preloaded in the background, so paging mostly hits the cache.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use blick_core::look::{Look, Render};
use blick_core::{Adjustments, Pixels, Rotation};
use blick_immich::ThumbSize;
use gtk::{gdk, gio, glib};

use crate::crop_view::CropView;
use crate::ctx::{Ctx, texture};
use crate::edit::EditPanel;
use crate::looks::{self, Library};
use crate::photo::{Photo, Source};
use crate::runtime;
use crate::video::{VideoSource, VideoView};
use crate::zoom::ZoomView;

/// What applying the look needs; the baked part, if already there.
struct RenderJob {
    /// Pointer to the look and strength in percent, as the cache key.
    key: (usize, u32),
    look: Arc<Look>,
    strength: f32,
    cached: Option<Arc<Render>>,
}

impl RenderJob {
    fn render(self) -> Arc<Render> {
        self.cached
            .unwrap_or_else(|| Arc::new(self.look.render(self.strength)))
    }
}

pub struct Viewer {
    pub page: adw::NavigationPage,
    chrome: adw::ToolbarView,
    info_button: gtk::MenuButton,
    download: gtk::Button,
    immersive: Cell<bool>,
    hide_source: RefCell<Option<glib::SourceId>>,
    ctx: Rc<Ctx>,
    toasts: adw::ToastOverlay,
    zoom: Rc<ZoomView>,
    /// Picture (zoom) or video, depending on the current entry.
    stack: gtk::Stack,
    video: Rc<VideoView>,
    title: adw::WindowTitle,
    split: adw::OverlaySplitView,
    edit_toggle: gtk::ToggleButton,
    prev: gtk::Button,
    next: gtk::Button,
    edit: Rc<EditPanel>,
    model: RefCell<Option<gio::ListModel>>,
    index: Cell<u32>,
    generation: Cell<u64>,
    /// Current preview without adjustments, starting point for the live view.
    base: RefCell<Option<(Arc<Pixels>, gdk::Texture)>>,
    /// Last computed preview with adjustments, for the before/after comparison.
    edited: RefCell<Option<gdk::Texture>>,
    /// A long click is currently holding the original in view.
    showing_original: Cell<bool>,
    original_badge: gtk::Box,
    loading_badge: gtk::Box,
    edit_busy: Cell<bool>,
    edit_dirty: Cell<bool>,
    library: Rc<Library>,
    /// Baked look for the current look and strength (pointer, percent).
    render_cache: RefCell<Option<(usize, u32, Arc<Render>)>>,
    /// Frame for cropping, sits over the canvas in crop mode.
    crop_view: Rc<CropView>,
    crop_bar: gtk::Box,
    zoom_bar: gtk::Box,
    /// Crop before the frame was opened, for cancel.
    crop_before: Cell<Option<Option<blick_core::Crop>>>,
    /// Dimensions of the original (rotated).
    dims: Cell<Option<(u32, u32)>>,
    on_files_changed: RefCell<Option<Box<dyn Fn()>>>,
}

fn osd_button(icon: &str, tooltip: &str, align: gtk::Align) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(align)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .build();
    b.set_css_classes(&["osd", "circular"]);
    b.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    b
}

impl Viewer {
    pub fn new(ctx: Rc<Ctx>, toasts: adw::ToastOverlay, library: Rc<Library>) -> Rc<Self> {
        let zoom = ZoomView::new();
        let prev = osd_button(
            "go-previous-symbolic",
            "Previous picture (left arrow)",
            gtk::Align::Start,
        );
        let next = osd_button(
            "go-next-symbolic",
            "Next picture (right arrow)",
            gtk::Align::End,
        );

        let video = VideoView::new();
        let stack = gtk::Stack::new();
        stack.add_named(&zoom.widget, Some("bild"));
        stack.add_named(&video.widget, Some("video"));
        let stage = gtk::Overlay::new();
        stage.set_child(Some(&stack));
        stage.add_overlay(&prev);
        stage.add_overlay(&next);
        // Floating bar in the libadwaita style (`toolbar` + `osd`), like
        // the zoom buttons in the GNOME image viewer mockups.
        let original_badge = gtk::Box::builder()
            .css_classes(["toolbar", "osd"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(12)
            .margin_top(12)
            .visible(false)
            .can_target(false)
            .build();
        let badge_label = gtk::Label::builder()
            .label("Original")
            .margin_start(6)
            .margin_end(6)
            .build();
        badge_label.add_css_class("heading");
        original_badge.append(&badge_label);
        // Notice while the full resolution is still loading (bottom left).
        let loading_badge = gtk::Box::builder()
            .css_classes(["toolbar", "osd"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_start(12)
            .margin_bottom(12)
            .spacing(6)
            .visible(false)
            .can_target(false)
            .build();
        loading_badge.append(&gtk::Spinner::builder().spinning(true).margin_start(6).build());
        loading_badge.append(
            &gtk::Label::builder()
                .label("Loading full resolution …")
                .margin_end(6)
                .build(),
        );
        let zoom_bar = gtk::Box::builder()
            .css_classes(["toolbar", "osd"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        for (icon, tip, action) in [
            ("zoom-out-symbolic", "Zoom out (−)", "viewer.zoom-out"),
            ("zoom-fit-best-symbolic", "Fit to window (0)", "viewer.zoom-fit"),
            ("zoom-in-symbolic", "Zoom in (+)", "viewer.zoom-in"),
        ] {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .action_name(action)
                .build();
            b.add_css_class("flat");
            b.update_property(&[gtk::accessible::Property::Label(tip)]);
            zoom_bar.append(&b);
        }
        stage.add_overlay(&zoom_bar);
        stage.add_overlay(&original_badge);
        stage.add_overlay(&loading_badge);

        // Crop: the whole image with a frame, Cancel/Apply at the bottom.
        let crop_view = CropView::new();
        stage.add_overlay(&crop_view.widget);
        let crop_bar = gtk::Box::builder()
            .css_classes(["toolbar", "osd"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .spacing(6)
            .visible(false)
            .build();
        for (label, tip, action, class) in [
            ("Cancel", "Cancel (Esc)", "viewer.crop-cancel", None),
            (
                "Whole image",
                "Frame around the whole image",
                "viewer.crop-reset",
                None,
            ),
            (
                "Crop",
                "Apply (Enter)",
                "viewer.crop-apply",
                Some("suggested-action"),
            ),
        ] {
            let b = gtk::Button::builder()
                .label(label)
                .tooltip_text(tip)
                .action_name(action)
                .build();
            if let Some(c) = class {
                b.add_css_class(c);
            }
            crop_bar.append(&b);
        }
        stage.add_overlay(&crop_bar);

        let edit = EditPanel::new();
        let split = adw::OverlaySplitView::builder()
            .content(&stage)
            .sidebar(&edit.widget)
            .sidebar_position(gtk::PackType::End)
            .show_sidebar(false)
            .min_sidebar_width(260.0)
            .max_sidebar_width(320.0)
            .build();

        let title = adw::WindowTitle::new("", "");
        let edit_toggle = gtk::ToggleButton::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit (E)")
            .build();
        edit_toggle
            .bind_property("active", &split, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        header.pack_end(&edit_toggle);
        let info_button = gtk::MenuButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Picture info (Alt+Enter)")
            .build();
        header.pack_end(&info_button);
        let download = gtk::Button::builder()
            .icon_name("folder-download-symbolic")
            .tooltip_text("Download original from Immich")
            .visible(false)
            .build();
        header.pack_end(&download);
        header.pack_end(
            &gtk::Button::builder()
                .icon_name("object-rotate-right-symbolic")
                .tooltip_text("Rotate right (Ctrl+R)")
                .action_name("win.rotate-right")
                .build(),
        );
        header.pack_end(
            &gtk::Button::builder()
                .icon_name("object-rotate-left-symbolic")
                .tooltip_text("Rotate left (Ctrl+Shift+R)")
                .action_name("win.rotate-left")
                .build(),
        );

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&split));

        let page = adw::NavigationPage::builder()
            .title("Picture")
            .tag("viewer")
            .child(&view)
            .build();

        let this = Rc::new(Self {
            page,
            chrome: view.clone(),
            info_button: info_button.clone(),
            download: download.clone(),
            immersive: Cell::new(false),
            hide_source: RefCell::new(None),
            ctx,
            toasts,
            zoom,
            stack,
            video,
            title,
            split,
            edit_toggle,
            prev,
            next,
            edit,
            model: RefCell::new(None),
            index: Cell::new(0),
            generation: Cell::new(0),
            base: RefCell::new(None),
            edited: RefCell::new(None),
            showing_original: Cell::new(false),
            original_badge,
            loading_badge,
            edit_busy: Cell::new(false),
            edit_dirty: Cell::new(false),
            library,
            render_cache: RefCell::new(None),
            crop_view,
            crop_bar,
            zoom_bar,
            crop_before: Cell::new(None),
            dims: Cell::new(None),
            on_files_changed: RefCell::new(None),
        });

        this.video.connect_error(glib::clone!(
            #[weak]
            this,
            move |e| this.toast(&format!("Can't play video: {e}"))
        ));
        // Whoever goes back to the sheet doesn't want a video still playing in the background.
        this.page.connect_hidden(glib::clone!(
            #[weak]
            this,
            move |_| this.video.stop()
        ));
        this.zoom.connect_need_full(glib::clone!(
            #[weak]
            this,
            move || this.load_full()
        ));
        // When zoomed the arrows get in the way over the picture; paging still works by keyboard.
        this.zoom.connect_zoom_changed(glib::clone!(
            #[weak]
            this,
            move |zoomed| {
                let i = this.index.get();
                this.prev.set_visible(!zoomed && i > 0);
                this.next.set_visible(!zoomed && i + 1 < this.len());
            }
        ));
        // While editing: holding the left mouse button shows the original,
        // letting go shows the edited picture again.
        let hold = gtk::GestureLongPress::new();
        hold.set_button(gdk::BUTTON_PRIMARY);
        hold.connect_pressed(glib::clone!(
            #[weak]
            this,
            move |_, _, _| this.show_original(true)
        ));
        hold.connect_end(glib::clone!(
            #[weak]
            this,
            move |_, _| this.show_original(false)
        ));
        this.zoom.widget.add_controller(hold);
        this.prev.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.step(-1)
        ));
        this.next.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.step(1)
        ));
        this.edit.connect_changed(glib::clone!(
            #[weak]
            this,
            move || this.schedule_edit_preview()
        ));
        this.edit.save_copy.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.save(false)
        ));
        this.edit.remember.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.remember()
        ));
        this.edit.overwrite.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.save(true)
        ));
        this.edit.choose_look.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.choose_look()
        ));
        this.edit.import_looks.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| this.import_looks()
        ));
        this.edit.export_lut.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| this.export_lut()
        ));
        this.crop_view.connect_changed(glib::clone!(
            #[weak]
            this,
            move |c| this.edit.show_crop_size(Some(c))
        ));
        this.edit.crop_button.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.enter_crop()
        ));
        // Different preset: open the frame if closed, and fit again.
        this.edit.connect_aspect_changed(glib::clone!(
            #[weak]
            this,
            move || {
                if !this.cropping() {
                    this.enter_crop();
                }
                let (a, portrait) = this.edit.aspect();
                this.crop_view.set_aspect(a, portrait, true);
            }
        ));

        // Capture: the shortcuts should work even when focus is still on a
        // header bar button. If a slider of the edit bar has focus, it gets
        // the keys (arrows change the value there).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| {
                if this.cropping()
                    && this.edit.widget.focus_child().is_none()
                    && !mods.contains(gdk::ModifierType::CONTROL_MASK)
                {
                    match key {
                        gdk::Key::Return | gdk::Key::KP_Enter => this.finish_crop(true),
                        gdk::Key::Escape => this.finish_crop(false),
                        // Arrows belong to the frame as long as no button has focus.
                        _ if this.crop_bar.focus_child().is_none()
                            && this
                                .crop_view
                                .key(key, mods.contains(gdk::ModifierType::SHIFT_MASK)) => {}
                        _ => return glib::Propagation::Proceed,
                    }
                    return glib::Propagation::Stop;
                }
                if this.edit.widget.focus_child().is_some() {
                    return glib::Propagation::Proceed;
                }
                // Shift doesn't matter (on many layouts "+" sits on it).
                if mods.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SUPER_MASK,
                ) {
                    return glib::Propagation::Proceed;
                }
                if this.video.is_open() {
                    match key {
                        gdk::Key::space | gdk::Key::k => {
                            this.video.toggle_play();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::m => {
                            this.video.toggle_mute();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::Up => {
                            this.video.step_volume(1.0);
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::Down => {
                            this.video.step_volume(-1.0);
                            return glib::Propagation::Stop;
                        }
                        // There is no zoom for video.
                        gdk::Key::plus
                        | gdk::Key::KP_Add
                        | gdk::Key::equal
                        | gdk::Key::minus
                        | gdk::Key::KP_Subtract
                        | gdk::Key::_0
                        | gdk::Key::KP_0
                        | gdk::Key::_1
                        | gdk::Key::KP_1 => return glib::Propagation::Stop,
                        _ => {}
                    }
                }
                match key {
                    gdk::Key::Left => this.step(-1),
                    gdk::Key::Right => this.step(1),
                    gdk::Key::plus | gdk::Key::KP_Add | gdk::Key::equal => this.zoom.zoom_in(),
                    gdk::Key::minus | gdk::Key::KP_Subtract => this.zoom.zoom_out(),
                    gdk::Key::_0 | gdk::Key::KP_0 => this.zoom.fit(),
                    gdk::Key::_1 | gdk::Key::KP_1 => this.zoom.actual_size(),
                    // Space: close the quick look again, back to the sheet.
                    gdk::Key::space => {
                        let _ = this.page.activate_action("navigation.pop", None);
                    }
                    gdk::Key::s if this.edit_toggle.is_sensitive() => this.choose_look(),
                    gdk::Key::f => {
                        let _ = this.page.activate_action("win.fullscreen", None);
                    }
                    gdk::Key::e if this.edit_toggle.is_sensitive() => {
                        this.edit_toggle.set_active(!this.edit_toggle.is_active())
                    }
                    gdk::Key::c if this.edit_toggle.is_sensitive() => this.enter_crop(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        this.page.add_controller(keys);

        // Zoom buttons bottom right: same paths as +, − and 0.
        let actions = gio::SimpleActionGroup::new();
        for (name, f) in [
            ("zoom-in", ZoomView::zoom_in as fn(&ZoomView)),
            ("zoom-out", ZoomView::zoom_out),
            ("zoom-fit", ZoomView::fit),
        ] {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(glib::clone!(
                #[weak]
                this,
                move |_, _| f(&this.zoom)
            ));
            actions.add_action(&a);
        }
        for (name, f) in [
            (
                "crop-apply",
                (|v: &Rc<Viewer>| v.finish_crop(true)) as fn(&Rc<Viewer>),
            ),
            ("crop-cancel", |v| v.finish_crop(false)),
            ("crop-reset", |v| v.crop_view.reset()),
        ] {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(glib::clone!(
                #[weak]
                this,
                move |_, _| f(&this)
            ));
            actions.add_action(&a);
        }
        this.page.insert_action_group("viewer", Some(&actions));

        this.download.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.download_original()
        ));
        // On open, focus goes to the picture, not the back button.
        this.zoom.widget.set_focusable(true);
        this.page.connect_shown(glib::clone!(
            #[weak]
            this,
            move |_| {
                this.zoom.widget.grab_focus();
            }
        ));
        let popover = gtk::Popover::new();
        this.info_button.set_popover(Some(&popover));
        popover.connect_show(glib::clone!(
            #[weak]
            this,
            move |p| this.fill_info(p)
        ));

        // In fullscreen the header bar hides and comes back on mouse movement.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak]
            this,
            move |_, _, _| this.poke_chrome()
        ));
        this.page.add_controller(motion);
        this
    }

    /// After deleting: show the next picture, or go back to the sheet if none is left.
    pub fn after_removal(self: &Rc<Self>) {
        let n = self.len();
        if n == 0 {
            let _ = self.page.activate_action("navigation.pop", None);
            return;
        }
        self.go(self.index.get().min(n - 1));
    }

    fn download_original(self: &Rc<Self>) {
        let (Some(photo), Some(client)) = (self.current(), self.ctx.immich()) else {
            return;
        };
        let Some(id) = photo.remote_id().map(str::to_owned) else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let name = {
                    let (c, asset) = (client.clone(), id.clone());
                    runtime::spawn(async move { Ok(c.original_file_name(&asset).await?) })
                        .await
                        .unwrap_or_else(|_| format!("{id}.jpg"))
                };
                let dialog = gtk::FileDialog::builder()
                    .title("Save original")
                    .initial_name(name.as_str())
                    .modal(true)
                    .build();
                if let Some(dir) = glib::user_special_dir(glib::UserDirectory::Pictures) {
                    dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
                }
                let Some(root) = this.page.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let Ok(Some(dst)) = dialog.save_future(Some(&root)).await.map(|f| f.path()) else {
                    return;
                };
                this.toast(&format!("Downloading {name} …"));
                let r =
                    runtime::spawn(async move { Ok(client.download_original(&id, &dst).await?) })
                        .await;
                match r {
                    Ok(bytes) => {
                        this.toast(&format!("{name} saved ({} MB)", bytes / 1_000_000))
                    }
                    Err(e) => this.toast(&format!("Download failed: {e:#}")),
                }
            }
        ));
    }

    /// Save sliders and look as `<image>.xmp` next to the picture.
    fn remember(self: &Rc<Self>) {
        let Some(photo) = self.current() else { return };
        let Some(path) = photo.path().map(Path::to_owned) else {
            return;
        };
        let adj = self.edit.adjustments();
        let look = self.edit.look();
        let settings = blick_core::sidecar::EditSettings {
            brightness: adj.brightness,
            contrast: adj.contrast,
            saturation: adj.saturation,
            look: look
                .as_ref()
                .and_then(|(l, _)| self.library.item_for(l))
                .map(|i| Library::key_of(&i)),
            look_strength: look.map_or(1.0, |(_, s)| s),
            crop: self.edit.crop(),
        };
        let task = gio::spawn_blocking(move || blick_core::sidecar::write_edit(&path, &settings));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match task.await {
                    Ok(Ok(())) => {
                        this.toast("Remembered. The edit will be back the next time you open it.");
                        // The contact sheet shows remembered edits, so it needs a new thumbnail.
                        photo.reload(&this.ctx);
                    }
                    Ok(Err(e)) => this.toast(&format!("Not remembered: {e}")),
                    Err(_) => this.toast("Not remembered."),
                }
            }
        ));
    }

    fn load_dimensions(self: &Rc<Self>, path: PathBuf, generation: u64) {
        let task = gio::spawn_blocking(move || blick_core::decode::dimensions(&path));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Ok(Some((w, h))) = task.await
                    && this.generation.get() == generation
                {
                    this.dims.set(Some((w, h)));
                    this.edit.set_dimensions(Some((w, h)));
                    this.update_natural();
                }
            }
        ));
    }

    /// Apply remembered settings, if there is a sidecar.
    fn load_sidecar(self: &Rc<Self>, path: PathBuf, generation: u64) {
        let task = gio::spawn_blocking(move || blick_core::sidecar::read_edit(&path));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(Some(e)) = task.await else { return };
                if this.generation.get() != generation {
                    return;
                }
                let adj = Adjustments {
                    brightness: e.brightness,
                    contrast: e.contrast,
                    saturation: e.saturation,
                };
                this.edit.set_values(adj, e.look_strength);
                this.edit.set_crop(e.crop);
                this.update_natural();
                let item = e.look.as_deref().and_then(|k| this.library.find(k));
                match item {
                    Some(item) => match looks::load(&item).await {
                        Ok(look) if this.generation.get() == generation => {
                            this.edit.set_look(Some(look))
                        }
                        Ok(_) => {}
                        Err(err) => this.toast(&format!("Remembered look is missing: {err:#}")),
                    },
                    None if e.look.is_some() => {
                        this.toast("The remembered look is no longer in the library.")
                    }
                    None => this.edit.set_look(None),
                }
            }
        ));
    }

    pub fn toggle_info(&self) {
        if let Some(p) = self.info_button.popover() {
            if p.is_visible() {
                p.popdown();
            } else {
                p.popup();
            }
        }
    }

    fn fill_info(self: &Rc<Self>, popover: &gtk::Popover) {
        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(18)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        popover.set_child(Some(&grid));
        let Some(photo) = self.current() else { return };
        let title = gtk::Label::builder()
            .label(photo.title())
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(36)
            .build();
        title.add_css_class("heading");
        grid.attach(&title, 0, 0, 2, 1);
        if photo.is_video() {
            let mut rows = self.video.info_rows();
            match photo.path() {
                Some(p) => {
                    if let Ok(m) = std::fs::metadata(p) {
                        rows.push((
                            "File size",
                            format!("{:.1} MB", m.len() as f64 / 1e6),
                        ));
                    }
                }
                None => rows.push(("Source", "Immich (playback version)".to_owned())),
            }
            for (i, (k, v)) in rows.into_iter().enumerate() {
                let key = gtk::Label::builder().label(k).xalign(1.0).build();
                key.add_css_class("dim-label");
                let val = gtk::Label::builder()
                    .label(v)
                    .xalign(0.0)
                    .selectable(true)
                    .build();
                grid.attach(&key, 0, i as i32 + 1, 1, 1);
                grid.attach(&val, 1, i as i32 + 1, 1, 1);
            }
            return;
        }
        let Some(path) = photo.path().map(Path::to_owned) else {
            let l = gtk::Label::builder()
                .label("Lives in Immich")
                .xalign(0.0)
                .build();
            l.add_css_class("dim-label");
            grid.attach(&l, 0, 1, 2, 1);
            return;
        };
        let task = gio::spawn_blocking(move || blick_core::info::read(&path));
        glib::spawn_future_local(async move {
            let Ok(info) = task.await else { return };
            for (i, (k, v)) in info.rows().into_iter().enumerate() {
                let key = gtk::Label::builder().label(k).xalign(1.0).build();
                key.add_css_class("dim-label");
                let val = gtk::Label::builder()
                    .label(v)
                    .xalign(0.0)
                    .selectable(true)
                    .build();
                grid.attach(&key, 0, i as i32 + 1, 1, 1);
                grid.attach(&val, 1, i as i32 + 1, 1, 1);
            }
        });
    }

    pub fn set_immersive(self: &Rc<Self>, on: bool) {
        self.immersive.set(on);
        self.chrome.set_extend_content_to_top_edge(on);
        if on {
            self.poke_chrome();
        } else {
            if let Some(id) = self.hide_source.take() {
                id.remove();
            }
            self.chrome.set_reveal_top_bars(true);
        }
    }

    fn poke_chrome(self: &Rc<Self>) {
        if !self.immersive.get() {
            return;
        }
        self.chrome.set_reveal_top_bars(true);
        if let Some(id) = self.hide_source.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_secs(2),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.hide_source.take();
                    if this.immersive.get() {
                        this.chrome.set_reveal_top_bars(false);
                    }
                }
            ),
        );
        self.hide_source.replace(Some(id));
    }

    pub fn connect_files_changed(&self, f: impl Fn() + 'static) {
        self.on_files_changed.replace(Some(Box::new(f)));
    }

    pub fn index(&self) -> u32 {
        self.index.get()
    }

    pub fn current(&self) -> Option<Photo> {
        self.photo_at(self.index.get())
    }

    fn photo_at(&self, i: u32) -> Option<Photo> {
        self.model.borrow().as_ref()?.item(i).and_downcast()
    }

    fn len(&self) -> u32 {
        self.model.borrow().as_ref().map_or(0, |m| m.n_items())
    }

    pub fn show(self: &Rc<Self>, model: gio::ListModel, index: u32) {
        self.model.replace(Some(model));
        self.go(index);
    }

    fn step(self: &Rc<Self>, delta: i64) {
        let target = self.index.get() as i64 + delta;
        if (0..self.len() as i64).contains(&target) {
            self.go(target as u32);
        }
    }

    fn go(self: &Rc<Self>, index: u32) {
        let Some(photo) = self.photo_at(index) else {
            return;
        };
        if self.cropping() {
            self.finish_crop(false);
        }
        self.index.set(index);
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.loading_badge.set_visible(false);

        self.title.set_title(&photo.title());
        self.title
            .set_subtitle(&format!("{} of {}", index + 1, self.len()));
        self.zoom.show(photo.texture().as_ref(), None);
        self.zoom
            .picture()
            .update_property(&[gtk::accessible::Property::Label(&photo.title())]);
        self.prev.set_visible(index > 0);
        self.next.set_visible(index + 1 < self.len());

        self.base.replace(None);
        self.dims.set(None);
        self.edit.set_dimensions(None);
        self.edit.reset();
        let local = photo.path().is_some();
        self.download.set_visible(!local);

        if photo.is_video() {
            self.edit_toggle.set_sensitive(false);
            self.split.set_show_sidebar(false);
            self.zoom_bar.set_visible(false);
            self.stack.set_visible_child_name("video");
            let quelle = match photo.source() {
                Source::Local(path) => Some(VideoSource::File(path.clone())),
                Source::Remote { id, .. } => self.ctx.immich().map(|c| VideoSource::Http {
                    url: c.playback_url(id),
                    api_key: c.api_key().to_owned(),
                }),
            };
            match quelle {
                Some(q) => self.video.open(q, &photo.title()),
                None => self.toast("No connection to Immich."),
            }
            if local {
                self.prefetch(index);
            }
            return;
        }
        self.video.stop();
        self.zoom_bar.set_visible(true);
        self.stack.set_visible_child_name("bild");
        self.edit
            .set_overwrite_allowed(photo.path().is_some_and(blick_core::export::can_overwrite));
        self.edit_toggle.set_sensitive(local);
        if !local {
            self.split.set_show_sidebar(false);
        }

        match photo.source().clone() {
            Source::Local(path) => {
                self.load_sidecar(path.clone(), generation);
                self.load_dimensions(path.clone(), generation);
                self.load_local(path, generation);
                self.prefetch(index);
            }
            Source::Remote { id, .. } => self.load_remote(id, generation),
        }
    }

    /// Preview size: the larger window edge in device pixels, rounded to 512,
    /// so small resizes don't invalidate the cache.
    fn target_px(&self) -> u32 {
        let (w, h, scale) = match self.page.root() {
            Some(root) => (root.width(), root.height(), self.page.scale_factor()),
            None => (1920, 1080, 1),
        };
        let px = (w.max(h).max(800) * scale.max(1)) as u32;
        px.div_ceil(512).clamp(2, 8) * 512
    }

    fn load_local(self: &Rc<Self>, path: PathBuf, generation: u64) {
        let target = self.target_px();
        let key = (path.clone(), target);
        if let Some(px) = self.ctx.previews.get(&key) {
            self.set_base(px);
            return;
        }
        let cache = self.ctx.previews.clone();
        let task = gio::spawn_blocking(move || {
            let px = Arc::new(blick_core::decode::load_preview(&path, target, target)?);
            cache.insert(key, px.clone());
            Ok::<_, blick_core::CoreError>(px)
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = task.await;
                if this.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(Ok(px)) => this.set_base(px),
                    Ok(Err(e)) => this.toast(&format!("Can't open picture: {e}")),
                    Err(_) => this.toast("Can't open picture."),
                }
            }
        ));
    }

    fn load_remote(self: &Rc<Self>, id: String, generation: u64) {
        let Some(client) = self.ctx.immich() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let id_full = id.clone();
                let result = runtime::spawn(async move {
                    let bytes = client.thumbnail(&id, ThumbSize::Preview).await?;
                    Ok(
                        tokio::task::spawn_blocking(move || blick_core::decode::load_bytes(&bytes))
                            .await??,
                    )
                })
                .await;
                if this.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(px) => {
                        let preview_edge = px.width.max(px.height);
                        this.zoom.upgrade(&texture(px));
                        this.load_remote_full(id_full, preview_edge, generation);
                    }
                    Err(e) => this.toast(&format!("Immich preview missing: {e:#}")),
                }
            }
        ));
    }

    /// After the preview, the full resolution. First `size=fullsize`: for JPEG and
    /// the like Immich redirects to the original. For HEIC and RAW you only get it
    /// if the server generates fullsize images; otherwise the preview comes back,
    /// and Blick fetches the original and decodes it itself (glycin, rawler).
    fn load_remote_full(self: &Rc<Self>, id: String, preview_edge: u32, generation: u64) {
        let Some(client) = self.ctx.immich() else {
            return;
        };
        let target = self.target_px();
        let tmp_dir = glib::user_cache_dir()
            .join("blick")
            .join("immich-originals");
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let current = |this: &Rc<Self>| this.generation.get() == generation;
                this.loading_badge.set_visible(true);
                // Hides on every exit, unless a newer picture has taken over.
                struct HideOnDrop(Rc<Viewer>, u64);
                impl Drop for HideOnDrop {
                    fn drop(&mut self) {
                        if self.0.generation.get() == self.1 {
                            self.0.loading_badge.set_visible(false);
                        }
                    }
                }
                let _hide = HideOnDrop(this.clone(), generation);
                let c = client.clone();
                let i = id.clone();
                let bytes =
                    runtime::spawn(async move { Ok(c.thumbnail(&i, ThumbSize::Fullsize).await?) })
                        .await;
                if !current(&this) {
                    return;
                }
                let decoded = match bytes {
                    Ok(b) => gio::spawn_blocking(move || blick_core::decode::load_bytes(&b))
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .filter(|px| px.width.max(px.height) > preview_edge),
                    Err(e) => {
                        tracing::debug!("fullsize {id}: {e:#}");
                        None
                    }
                };
                let px = match decoded {
                    Some(px) => px,
                    None => {
                        // Load the original into the cache, decode it there, then drop it.
                        let c = client.clone();
                        let i = id.clone();
                        let dir = tmp_dir.clone();
                        let file = runtime::spawn(async move {
                            let name = c.original_file_name(&i).await?;
                            let ext = std::path::Path::new(&name)
                                .extension()
                                .map(|e| e.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            std::fs::create_dir_all(&dir)?;
                            let dst = dir.join(format!("{i}.{ext}"));
                            c.download_original(&i, &dst).await?;
                            Ok(dst)
                        })
                        .await;
                        if !current(&this) {
                            if let Ok(f) = &file {
                                let _ = std::fs::remove_file(f);
                            }
                            return;
                        }
                        let file = match file {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!("original {id}: {e:#}");
                                return;
                            }
                        };
                        let decoded = gio::spawn_blocking(move || {
                            let r = blick_core::decode::load_oriented(&file)
                                .map(blick_core::Pixels::from_dynamic);
                            let _ = std::fs::remove_file(&file);
                            r
                        })
                        .await;
                        match decoded {
                            Ok(Ok(px)) if px.width.max(px.height) > preview_edge => px,
                            Ok(Err(e)) => {
                                tracing::warn!("original {id} unreadable: {e}");
                                return;
                            }
                            _ => return,
                        }
                    }
                };
                if !current(&this) {
                    return;
                }
                tracing::debug!("Immich {id}: full resolution {}x{}", px.width, px.height);
                // First a sharp version at screen size, then the original for zooming.
                let full = Arc::new(px);
                let f = full.clone();
                let fit =
                    gio::spawn_blocking(move || (*f).clone().fit_within(target, target)).await;
                if !current(&this) {
                    return;
                }
                if let Ok(Ok(fit)) = fit {
                    this.zoom.upgrade(&texture(fit));
                }
                let full = Arc::try_unwrap(full).unwrap_or_else(|a| (*a).clone());
                this.zoom.upgrade(&texture(full));
            }
        ));
    }

    /// Full resolution for zooming. With edits the edited preview stays
    /// in place, otherwise you'd see the unedited original when zooming in.
    fn load_full(self: &Rc<Self>) {
        let Some(path) = self.current().and_then(|p| p.path().map(Path::to_owned)) else {
            return;
        };
        let generation = self.generation.get();
        let task = gio::spawn_blocking(move || {
            blick_core::decode::load_oriented(&path).map(blick_core::Pixels::from_dynamic)
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let r = task.await;
                if let Ok(Err(e)) = &r {
                    tracing::warn!("full resolution: {e}");
                }
                if let Ok(Ok(px)) = r
                    && this.generation.get() == generation
                    && this.edit.is_identity()
                {
                    this.zoom.upgrade(&texture(px));
                }
            }
        ));
    }

    /// The two pictures ahead and one back, in that order.
    fn prefetch(&self, index: u32) {
        let target = self.target_px();
        for i in [index as i64 + 1, index as i64 - 1, index as i64 + 2] {
            let Some(path) = u32::try_from(i)
                .ok()
                .and_then(|i| self.photo_at(i))
                .filter(|p| !p.is_video())
                .and_then(|p| p.path().map(Path::to_owned))
            else {
                continue;
            };
            let key = (path.clone(), target);
            if self.ctx.previews.get(&key).is_some() {
                continue;
            }
            let cache = self.ctx.previews.clone();
            drop(gio::spawn_blocking(move || {
                if let Ok(px) = blick_core::decode::load_preview(&path, target, target) {
                    cache.insert(key, Arc::new(px));
                }
            }));
        }
    }

    fn set_base(self: &Rc<Self>, px: Arc<Pixels>) {
        let tex = texture((*px).clone());
        self.zoom.upgrade(&tex);
        self.base.replace(Some((px, tex)));
        self.edited.replace(None);
        self.schedule_edit_preview();
    }

    fn show_original(self: &Rc<Self>, on: bool) {
        if on {
            if !self.edit_toggle.is_active() || self.edit.is_identity() || self.cropping() {
                return;
            }
            let Some((_, tex)) = self.base.borrow().clone() else {
                return;
            };
            self.showing_original.set(true);
            self.original_badge.set_visible(true);
            self.zoom.replace(Some(&tex));
        } else if self.showing_original.replace(false) {
            self.original_badge.set_visible(false);
            let edited = self.edited.borrow().clone();
            match edited {
                Some(tex) if !self.edit.is_identity() => self.zoom.replace(Some(&tex)),
                _ => self.schedule_edit_preview(),
            }
        }
    }

    /// Applies the sliders to the preview. If a computation is already running,
    /// it recomputes exactly once afterwards with the latest state.
    /// Fetch the baked look or have it baked. Baking costs up to ~60 ms
    /// (big HaldCLUT), so only when the look or strength changed.
    fn render_job(&self) -> Option<RenderJob> {
        let (look, strength) = self.edit.look()?;
        let key = (
            Arc::as_ptr(&look) as usize,
            (strength * 100.0).round() as u32,
        );
        let cached = self
            .render_cache
            .borrow()
            .as_ref()
            .filter(|c| (c.0, c.1) == key)
            .map(|c| c.2.clone());
        Some(RenderJob {
            key,
            look,
            strength,
            cached,
        })
    }

    fn seed(&self) -> u64 {
        self.current()
            .and_then(|p| p.path().map(blick_core::look::seed_for))
            .unwrap_or(0)
    }

    /// Applies sliders and look to the preview. If a computation is already running,
    /// it recomputes exactly once afterwards with the latest state.
    fn schedule_edit_preview(self: &Rc<Self>) {
        let Some((px, tex)) = self.base.borrow().clone() else {
            return;
        };
        if self.preview_is_plain() {
            self.edited.replace(None);
            self.show_preview(&tex);
            return;
        }
        if self.edit_busy.get() {
            self.edit_dirty.set(true);
            return;
        }
        self.edit_busy.set(true);
        let adj = self.edit.adjustments();
        let job = self.render_job();
        let seed = self.seed();
        let crop = self.preview_crop();
        let generation = self.generation.get();
        let task = gio::spawn_blocking(move || {
            let mut out = match crop {
                Some(c) => c.apply(&px),
                None => (*px).clone(),
            };
            blick_core::adjust::apply(&mut out.data, adj);
            let render = job.map(|j| {
                let key = j.key;
                let r = j.render();
                r.apply(&mut out, seed);
                (key.0, key.1, r)
            });
            (out, render)
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = task.await;
                this.edit_busy.set(false);
                if let Ok((px, render)) = result {
                    if let Some(r) = render {
                        this.render_cache.replace(Some(r));
                    }
                    // State no longer matches (frame opened/closed, everything reset):
                    // the recompute below catches up.
                    let fits = this.preview_crop() == crop && !this.preview_is_plain();
                    if this.generation.get() == generation && fits {
                        let tex = texture(px);
                        this.show_preview(&tex);
                        this.edited.replace(Some(tex));
                    } else if this.generation.get() == generation {
                        this.edit_dirty.set(true);
                    }
                }
                if this.edit_dirty.replace(false) {
                    this.schedule_edit_preview();
                }
            }
        ));
    }

    fn choose_look(self: &Rc<Self>) {
        let base = self.base.borrow().as_ref().map(|(px, _)| px.clone());
        let w = Rc::downgrade(self);
        looks::pick(&self.page, self.library.clone(), base, move |item| {
            let Some(this) = w.upgrade() else { return };
            let Some(item) = item else {
                this.edit.set_look(None);
                return;
            };
            glib::spawn_future_local(async move {
                match looks::load(&item).await {
                    Ok(look) => {
                        if !look.ignored.is_empty() {
                            this.toast(&format!(
                                "{}: not applied: {}",
                                look.name,
                                look.ignored.join(", ")
                            ));
                        }
                        this.split.set_show_sidebar(true);
                        this.edit.set_look(Some(look));
                    }
                    Err(e) => this.toast(&format!("Can't load look: {e:#}")),
                }
            });
        });
    }

    fn import_looks(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Looks (.cube, .png, .xmp)"));
        for p in ["*.cube", "*.CUBE", "*.png", "*.PNG", "*.xmp", "*.XMP"] {
            filter.add_pattern(p);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import looks")
            .filters(&filters)
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Some(root) = this.page.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let Ok(files) = dialog.open_multiple_future(Some(&root)).await else {
                    return;
                };
                let paths: Vec<PathBuf> = files
                    .iter::<gio::File>()
                    .flatten()
                    .filter_map(|f| f.path())
                    .collect();
                this.run_import(paths).await;
            }
        ));
    }

    pub fn reload_looks(&self) {
        self.library.reload();
    }

    pub async fn run_import(&self, paths: Vec<PathBuf>) {
        let report = gio::spawn_blocking(move || looks::import(&paths)).await;
        let Ok(report) = report else {
            self.toast("Import crashed.");
            return;
        };
        self.library.reload();
        let mut text = format!("{} looks imported", report.imported);
        if !report.failed.is_empty() {
            for (p, e) in &report.failed {
                tracing::warn!("look {}: {e}", p.display());
            }
            text.push_str(&format!(", {} unreadable", report.failed.len()));
        }
        if !report.ignored.is_empty() {
            let names: Vec<String> = report
                .ignored
                .iter()
                .map(|(n, f)| format!("{n} ({})", f.join(", ")))
                .collect();
            text.push_str(&format!(". Not applied: {}", names.join("; ")));
        }
        self.toasts
            .add_toast(adw::Toast::builder().title(text).timeout(8).build());
    }

    /// Sliders and look as `.cube`. Grain and vignette can't be expressed in any LUT format.
    fn export_lut(self: &Rc<Self>) {
        let adj = self.edit.adjustments();
        let look = self.edit.look();
        let name = look
            .as_ref()
            .map_or("Blick".to_owned(), |(l, _)| l.name.clone());
        let dialog = gtk::FileDialog::builder()
            .title("Export as LUT")
            .initial_name(format!("{name}.cube"))
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Some(root) = this.page.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let Ok(Some(path)) = dialog.save_future(Some(&root)).await.map(|f| f.path()) else {
                    return;
                };
                let job = gio::spawn_blocking(move || {
                    let lut = bake_lut(adj, look);
                    std::fs::write(&path, lut.to_cube(&name)).map(|_| path)
                });
                match job.await {
                    Ok(Ok(path)) => this.toast(&format!(
                        "Saved as {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )),
                    Ok(Err(e)) => this.toast(&format!("Not saved: {e}")),
                    Err(_) => this.toast("Not saved."),
                }
            }
        ));
    }

    fn save(self: &Rc<Self>, overwrite: bool) {
        let Some(photo) = self.current() else { return };
        let Some(src) = photo.path().map(Path::to_owned) else {
            return;
        };
        let adj = self.edit.adjustments();
        let job = self.render_job();
        let crop = self.edit.crop();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if overwrite && !this.confirm_overwrite(&photo.title()).await {
                    return;
                }
                let dst = if overwrite {
                    src.clone()
                } else {
                    blick_core::export::copy_path(&src)
                };
                let task = {
                    let (src, dst) = (src.clone(), dst.clone());
                    gio::spawn_blocking(move || {
                        let render = job.map(RenderJob::render);
                        blick_core::export::save_adjusted(
                            &src,
                            &dst,
                            crop,
                            adj,
                            render.as_deref(),
                        )?;
                        // If the edit now lives in the original, a remembered
                        // state must not apply it a second time.
                        if src == dst {
                            blick_core::sidecar::clear_edit(&src)?;
                        }
                        Ok::<_, blick_core::CoreError>(())
                    })
                };
                match task.await {
                    Ok(Ok(())) => {
                        let name = dst
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        this.toast(&format!("Saved as {name}"));
                        if overwrite {
                            this.forget(&photo, &src);
                            this.go(this.index.get());
                        }
                        if let Some(f) = this.on_files_changed.borrow().as_ref() {
                            f();
                        }
                    }
                    Ok(Err(e)) => this.toast(&format!("Not saved: {e}")),
                    Err(_) => this.toast("Not saved: the save operation crashed."),
                }
            }
        ));
    }

    async fn confirm_overwrite(&self, name: &str) -> bool {
        let dialog = adw::AlertDialog::new(
            Some("Overwrite original?"),
            Some(&format!(
                "Crop and adjustments will be written into {name} for good. This can't be undone."
            )),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("overwrite", "Overwrite")]);
        dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.choose_future(Some(&self.page)).await == "overwrite"
    }

    /// Discard caches for a changed file and fetch the thumbnail again.
    fn forget(&self, photo: &Photo, path: &Path) {
        self.ctx.previews.remove_where(|k| k.0 == path);
        photo.reload(&self.ctx);
    }

    pub fn rotate(self: &Rc<Self>, rot: Rotation) {
        if self.cropping() {
            self.finish_crop(false);
        }
        let Some(photo) = self.current() else { return };
        if photo.is_video() {
            return;
        }
        let Some(path) = photo.path().map(Path::to_owned) else {
            return;
        };
        let task = {
            let path = path.clone();
            gio::spawn_blocking(move || blick_core::rotate::rotate_file(&path, rot))
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match task.await {
                    Ok(Ok(_)) => {
                        this.forget(&photo, &path);
                        if this.current().as_ref() == Some(&photo) {
                            this.go(this.index.get());
                        }
                    }
                    Ok(Err(e)) => this.toast(&format!("Not rotated: {e}")),
                    Err(_) => this.toast("Not rotated: the operation crashed."),
                }
            }
        ));
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    // --- Cropping ---------------------------------------------------------

    fn cropping(&self) -> bool {
        self.crop_view.widget.is_visible()
    }

    /// Crop for the preview: in frame mode the whole image.
    fn preview_crop(&self) -> Option<blick_core::Crop> {
        if self.cropping() {
            None
        } else {
            self.edit.crop()
        }
    }

    /// The preview needs no computation: nothing to crop, no colors.
    fn preview_is_plain(&self) -> bool {
        self.edit.colors_identity() && self.preview_crop().is_none()
    }

    /// Preview to wherever it is currently visible.
    fn show_preview(&self, tex: &gdk::Texture) {
        if self.cropping() {
            self.crop_view.set_texture(Some(tex));
        } else if !self.showing_original.get() {
            self.zoom.replace(Some(tex));
        }
    }

    /// Reference size for the zoom: the original, after the crop.
    fn update_natural(&self) {
        if let Some((w, h)) = self.dims.get() {
            let (w, h) = match self.edit.crop() {
                Some(c) => c.output_size(w, h),
                None => (w, h),
            };
            self.zoom.set_natural(w, h);
        }
    }

    fn enter_crop(self: &Rc<Self>) {
        if self.cropping() || !self.edit_toggle.is_sensitive() {
            return;
        }
        let Some((_, tex)) = self.base.borrow().clone() else {
            return;
        };
        self.show_original(false);
        self.split.set_show_sidebar(true);
        self.crop_before.set(Some(self.edit.crop()));
        self.zoom.fit();
        self.crop_view.set_texture(Some(&tex));
        self.crop_view
            .set_crop(self.edit.crop().unwrap_or(blick_core::Crop::FULL));
        self.crop_view.widget.set_visible(true);
        self.crop_bar.set_visible(true);
        for w in [
            self.zoom.widget.upcast_ref::<gtk::Widget>(),
            self.zoom_bar.upcast_ref(),
            self.prev.upcast_ref(),
            self.next.upcast_ref(),
        ] {
            w.set_visible(false);
        }
        // Without a previous crop, frame it in the chosen preset right away.
        // A remembered crop stays as it is until you drag.
        let (a, portrait) = self.edit.aspect();
        let fit = self.edit.crop().is_none();
        self.crop_view.set_aspect(a, portrait, fit);
        self.crop_view.grab_focus();
        self.schedule_edit_preview();
    }

    fn finish_crop(self: &Rc<Self>, apply: bool) {
        if !self.cropping() {
            return;
        }
        let before = self.crop_before.take().flatten();
        self.crop_view.widget.set_visible(false);
        self.crop_bar.set_visible(false);
        self.zoom.widget.set_visible(true);
        self.zoom_bar.set_visible(true);
        let i = self.index.get();
        self.prev.set_visible(i > 0);
        self.next.set_visible(i + 1 < self.len());
        self.edit.set_crop(if apply {
            Some(self.crop_view.crop())
        } else {
            before
        });
        self.edit.show_crop_size(self.edit.crop());
        self.zoom.fit();
        self.update_natural();
        self.zoom.widget.grab_focus();
        self.schedule_edit_preview();
    }
}

/// Sliders and look into a 33³ LUT. The sliders work on 8 bit, hence the detour through one pixel.
fn bake_lut(adj: Adjustments, look: Option<(Arc<Look>, f32)>) -> blick_core::look::Lut3d {
    let render = look.map(|(l, s)| l.render(s));
    blick_core::look::Lut3d::from_fn(blick_core::look::BAKE_SIZE, move |c| {
        let mut px = [0u8; 4];
        for i in 0..3 {
            px[i] = (c[i] * 255.0).round() as u8;
        }
        blick_core::adjust::apply(&mut px, adj);
        let rgb = [
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ];
        match render.as_ref().and_then(|r| r.lut.as_ref()) {
            Some(lut) => lut.sample(rgb),
            None => rgb,
        }
    })
}
