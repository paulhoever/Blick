//! Main window: sidebar with places and Immich albums, on the right the
//! contact sheet, with the single view on top when needed.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use blick_core::Rotation;
use gtk::{gio, glib};

use crate::config;
use crate::ctx::Ctx;
use crate::dupes_view::DupesView;
use crate::library_view::LibraryView;
use crate::photo::Photo;
use crate::runtime;
use crate::sheet::{Origin, Sheet, picture_count};
use crate::viewer::Viewer;

pub struct Window {
    win: adw::ApplicationWindow,
    ctx: Rc<Ctx>,
    settings: gio::Settings,
    toasts: adw::ToastOverlay,
    split: adw::NavigationSplitView,
    nav: adw::NavigationView,
    sheet: Rc<Sheet>,
    looks: Rc<crate::looks::Library>,
    viewer: Rc<Viewer>,
    dupes: Rc<DupesView>,
    places: gtk::ListBox,
    library: Rc<LibraryView>,
    albums: gtk::ListBox,
    /// Path per row in `places`; `None` is "Open folder …".
    place_paths: RefCell<Vec<Option<PathBuf>>>,
    /// Album ID per row in `albums`; `None` is the whole timeline.
    album_ids: RefCell<Vec<(Option<String>, String)>>,
    /// Single-file start (`blick picture.jpg`): viewer with exactly these files, no folder import.
    direct: RefCell<Option<gio::ListStore>>,
    /// Last opened folder, shown under "On this device".
    custom_place: RefCell<Option<PathBuf>>,
}

/// Mounted removable drives (USB, SD card) with a local path.
fn removable_mounts() -> Vec<(String, PathBuf)> {
    let monitor = gio::VolumeMonitor::get();
    monitor
        .mounts()
        .into_iter()
        .filter(|m| !m.is_shadowed())
        .filter(|m| {
            m.drive().is_some_and(|d| d.is_removable() || d.is_media_removable())
                || m.root()
                    .path()
                    .is_some_and(|p| p.starts_with("/run/media") || p.starts_with("/media"))
        })
        .filter_map(|m| Some((m.name().to_string(), m.root().path()?)))
        .collect()
}

fn sidebar_row(icon: &str, label: &str, suffix: Option<&str>) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Image::from_icon_name(icon));
    let l = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    b.append(&l);
    if let Some(s) = suffix {
        let c = gtk::Label::new(Some(s));
        c.set_css_classes(&["caption", "numeric", "dim-label"]);
        b.append(&c);
    }
    let row = gtk::ListBoxRow::builder().child(&b).build();
    // Without a name of its own, screen readers don't read the row aloud.
    let name = match suffix {
        Some(s) => format!("{label}, {s}"),
        None => label.to_owned(),
    };
    row.update_property(&[gtk::accessible::Property::Label(&name)]);
    row
}

fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_start(18)
        .margin_top(18)
        .margin_bottom(6)
        .build();
    l.add_css_class("heading");
    l.add_css_class("dim-label");
    l
}

impl Window {
    /// `recursive`: `start` is added as a library (`blick -r <folder>`).
    /// `files` not empty: show the pictures straight in the viewer, without loading the folder.
    pub fn open(
        app: &adw::Application,
        start: Option<PathBuf>,
        files: Vec<PathBuf>,
        recursive: bool,
    ) -> adw::ApplicationWindow {
        let ctx = Rc::new(Ctx::new());
        let settings = config::settings();
        let library = LibraryView::new(settings.clone());
        let toasts = adw::ToastOverlay::new();

        let sheet = Sheet::new(ctx.clone());
        let looks = crate::looks::Library::new();
        let viewer = Viewer::new(ctx.clone(), toasts.clone(), looks.clone());
        let nav = adw::NavigationView::new();
        nav.add(&sheet.page);

        let places = gtk::ListBox::new();
        places.add_css_class("navigation-sidebar");
        let albums = gtk::ListBox::new();
        albums.add_css_class("navigation-sidebar");

        let menu = gio::Menu::new();
        // At the top of the menu the light/dark switcher, as in the GNOME core apps.
        let theme_section = gio::Menu::new();
        let theme_item = gio::MenuItem::new(None, None);
        theme_item.set_attribute_value("custom", Some(&"theme".to_variant()));
        theme_section.append_item(&theme_item);
        menu.append_section(None, &theme_section);
        let tools = gio::Menu::new();
        tools.append(
            Some("Rescan library"),
            Some("win.library-rescan"),
        );
        tools.append(Some("Find duplicates …"), Some("win.find-duplicates"));
        tools.append(
            Some("Convert folder to JPEG XL …"),
            Some("win.convert-jxl"),
        );
        tools.append(
            Some("Import look folder …"),
            Some("win.looks-import-folder"),
        );
        tools.append(
            Some("Download film simulations"),
            Some("win.looks-download"),
        );
        menu.append_section(None, &tools);
        // Last section as in all GNOME apps: preferences, shortcuts, about.
        let app_section = gio::Menu::new();
        app_section.append(Some("Preferences"), Some("win.preferences"));
        app_section.append(Some("Keyboard shortcuts"), Some("win.shortcuts"));
        app_section.append(Some("About Blick"), Some("win.about"));
        menu.append_section(None, &app_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main menu")
            .primary(true)
            .build();
        if let Some(popover) = menu_button.popover().and_downcast::<gtk::PopoverMenu>() {
            popover.add_child(&crate::theme::selector(&settings), "theme");
        }
        let side_header = adw::HeaderBar::new();
        side_header.pack_end(&menu_button);

        let side_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side_box.append(&section_label("On this device"));
        side_box.append(&places);
        side_box.append(&section_label("Library"));
        side_box.append(&library.list);
        // The Immich section collapses: with many albums the sidebar would be endless otherwise.
        let immich_head = gtk::Button::builder()
            .css_classes(["flat"])
            .margin_top(12)
            .build();
        let head_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let head_label = section_label("Immich");
        head_label.set_margin_start(0);
        head_label.set_margin_top(0);
        head_label.set_margin_bottom(0);
        head_label.set_hexpand(true);
        // Arrow on the left, like the library folders.
        let head_icon = gtk::Image::from_icon_name("pan-down-symbolic");
        head_icon.add_css_class("dim-label");
        head_box.append(&head_icon);
        head_box.append(&head_label);
        immich_head.set_child(Some(&head_box));
        immich_head.set_margin_start(12);
        immich_head.set_margin_end(12);
        let expanded = settings.boolean("immich-expanded");
        albums.set_visible(expanded);
        head_icon.set_icon_name(Some(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
        immich_head.update_property(&[gtk::accessible::Property::Label(
            "Expand or collapse Immich albums",
        )]);
        immich_head.connect_clicked(glib::clone!(
            #[weak]
            albums,
            #[weak]
            head_icon,
            #[strong]
            settings,
            move |_| {
                let show = !albums.is_visible();
                albums.set_visible(show);
                head_icon.set_icon_name(Some(if show {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                }));
                let _ = settings.set_boolean("immich-expanded", show);
            }
        ));
        side_box.append(&immich_head);
        side_box.append(&albums);
        let side_view = adw::ToolbarView::new();
        side_view.add_top_bar(&side_header);
        side_view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&side_box)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        ));
        let sidebar = adw::NavigationPage::builder()
            .title("Blick")
            .child(&side_view)
            .build();
        let content = adw::NavigationPage::builder()
            .title("Pictures")
            .child(&nav)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&content)
            .min_sidebar_width(240.0)
            .max_sidebar_width(300.0)
            .sidebar_width_unit(adw::LengthUnit::Px)
            .build();
        toasts.set_child(Some(&split));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Blick")
            .default_width(settings.int("window-width"))
            .default_height(settings.int("window-height"))
            .maximized(settings.boolean("window-maximized"))
            .width_request(360)
            .height_request(294)
            .content(&toasts)
            .build();

        // Below 600 px width the sidebar collapses.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        win.add_breakpoint(bp);
        // Very narrow (phone): the selection bar shows only icons.
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            500.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        for c in &sheet.labeled_buttons {
            narrow.add_setter(c, "label", Some(&"".to_value()));
        }
        win.add_breakpoint(narrow);

        let dupes = DupesView::new(ctx.clone(), settings.clone(), toasts.clone(), win.clone());
        let this = Rc::new(Self {
            win: win.clone(),
            ctx,
            settings,
            toasts,
            split,
            nav,
            sheet,
            looks,
            viewer,
            dupes,
            places,
            library,
            albums,
            place_paths: RefCell::new(Vec::new()),
            album_ids: RefCell::new(Vec::new()),
            direct: RefCell::new(None),
            custom_place: RefCell::new(None),
        });
        this.settings
            .bind("include-subfolders", &this.sheet.subfolders, "active")
            .build();
        // On first start the Pictures folder is the library; if you remove it,
        // it doesn't come back.
        if !this.settings.boolean("library-initialized") {
            if let Some(pictures) = glib::user_special_dir(glib::UserDirectory::Pictures) {
                this.library.add_root(pictures);
            }
            let _ = this.settings.set_boolean("library-initialized", true);
        }
        this.setup_actions();
        this.setup_signals();
        this.fill_places(None);
        this.render_albums(None);
        this.connect_immich();

        if !files.is_empty() {
            this.open_direct(files);
            let keep = RefCell::new(Some(this));
            win.connect_close_request(move |_| {
                keep.take();
                glib::Propagation::Proceed
            });
            return win;
        }

        let folder = start
            .map(|p| {
                if p.is_file() {
                    p.parent().map(Path::to_owned).unwrap_or(p)
                } else {
                    p
                }
            })
            .or_else(|| {
                Some(PathBuf::from(this.settings.string("last-folder"))).filter(|p| p.is_dir())
            })
            .or_else(|| glib::user_special_dir(glib::UserDirectory::Pictures))
            .unwrap_or_else(glib::home_dir);
        if recursive {
            this.library.add_root(folder.clone());
        }
        this.open_folder(folder);

        // All signals hold the window object only weakly. This one strong
        // reference lives until the window is closed.
        let keep = RefCell::new(Some(this));
        win.connect_close_request(move |_| {
            keep.take();
            glib::Propagation::Proceed
        });
        win
    }

    /// Show pictures without a detour through the contact sheet. Only the way back loads the folder.
    fn open_direct(self: &Rc<Self>, files: Vec<PathBuf>) {
        let store = gio::ListStore::new::<Photo>();
        for (i, p) in files.into_iter().enumerate() {
            store.append(&Photo::local(p, i as u32 + 1));
        }
        self.direct.replace(Some(store.clone()));
        self.nav.push(&self.viewer.page);
        self.viewer.show(store.upcast(), 0);
        self.update_actions();
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn setup_actions(self: &Rc<Self>) {
        let w = self.weak();
        let action = |name: &str, f: fn(&Rc<Self>)| {
            let w = w.clone();
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| {
                if let Some(this) = w.upgrade() {
                    f(&this);
                }
            });
            self.win.add_action(&a);
        };
        action("open-folder", |this| this.choose_folder());
        action("rotate-left", |this| this.rotate(Rotation::Left));
        action("rotate-right", |this| this.rotate(Rotation::Right));
        action("upload", |this| this.upload());
        action("download", |this| this.download());
        action("share-link", |this| this.share_link());
        action("preferences", |this| this.preferences());
        action("about", |this| this.about());
        action("shortcuts", |this| {
            crate::shortcuts::dialog().present(Some(&this.win))
        });
        action("find-duplicates", |this| this.find_duplicates());
        action("library-add", |this| this.choose_library_folder());
        action("library-rescan", |this| this.library.rescan());
        action("info", |this| {
            if this.in_viewer() {
                this.viewer.toggle_info();
            }
        });
        action("trash", |this| this.trash());
        action("apply-look", |this| this.apply_look());
        action("fullscreen", |this| {
            if this.win.is_fullscreen() {
                this.win.unfullscreen();
            } else if this.in_viewer() {
                this.win.fullscreen();
            }
        });
        action("looks-import-folder", |this| this.import_look_folder());
        action("looks-download", |this| this.download_looks());
        action("convert-jxl", |this| this.convert_to_jxl());

        let remove = gio::SimpleAction::new("library-remove", Some(glib::VariantTy::STRING));
        let w2 = w.clone();
        remove.connect_activate(move |_, param| {
            if let (Some(this), Some(path)) = (w2.upgrade(), param.and_then(|p| p.get::<String>()))
            {
                this.library.remove_root(Path::new(&path));
            }
        });
        self.win.add_action(&remove);
        self.update_actions();
    }

    /// Convert the open folder with its subfolders to JPEG XL (menu, confirmation first).
    fn convert_to_jxl(self: &Rc<Self>) {
        let Some(Origin::Folder(dir)) = self.sheet.origin() else {
            self.toasts
                .add_toast(adw::Toast::new("Open a folder first."));
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let scan_dir = dir.clone();
                let Ok(files) = gio::spawn_blocking(move || crate::convert::scan(&scan_dir)).await
                else {
                    return;
                };
                if files.is_empty() {
                    this.toasts.add_toast(adw::Toast::new(
                        "No JPEG, PNG, WebP or HEIC pictures found.",
                    ));
                    return;
                }
                let [jpeg, png, webp, heic] = crate::convert::count_by_kind(&files);
                let body = format!(
                    "{} in \"{}\" and below: {jpeg} JPEG, {png} PNG, {webp} WebP, {heic} HEIC.\n\n\
                     JPEG and PNG are repacked losslessly (a JPEG can be restored bit for bit). \
                     WebP and HEIC are always converted (visually lossless, may get larger). \
                     Capture data (EXIF) is kept. \
                     Pictures with edits, a color profile or animation are left alone.",
                    picture_count(files.len()),
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dir.display().to_string()),
                );
                let trash = gtk::CheckButton::with_label("Move originals to the trash");
                trash.set_active(true);
                let dialog = adw::AlertDialog::builder()
                    .heading("Convert to JPEG XL?")
                    .body(body)
                    .extra_child(&trash)
                    .close_response("cancel")
                    .default_response("cancel")
                    .build();
                dialog.add_responses(&[("cancel", "Cancel"), ("run", "Convert")]);
                dialog.set_response_appearance("run", adw::ResponseAppearance::Suggested);
                if dialog.choose_future(Some(&this.win)).await != "run" {
                    return;
                }
                let trash_originals = trash.is_active();
                let saved = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let photos: Vec<Photo> = files
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| Photo::local(p, i as u32 + 1))
                    .collect();
                let counter = saved.clone();
                this.run_batch(
                    photos,
                    "Converting:",
                    move |p| crate::convert::convert_file(p, trash_originals, &counter),
                    move |this, _, report| {
                        let bytes = saved.load(std::sync::atomic::Ordering::Relaxed);
                        let mut text = format!(
                            "{} converted, {} saved",
                            picture_count(report.done.len()),
                            glib::format_size(bytes)
                        );
                        if !report.failed.is_empty() {
                            text.push_str(&format!(
                                ". {} stayed unchanged, e.g. {}",
                                report.failed.len(),
                                report.failed[0].1
                            ));
                        }
                        if report.cancelled > 0 {
                            text.push_str(&format!(", {} cancelled", report.cancelled));
                        }
                        this.toasts
                            .add_toast(adw::Toast::builder().title(text).timeout(10).build());
                        this.sheet.refresh();
                    },
                );
            }
        ));
    }

    fn find_duplicates(&self) {
        self.dupes.prepare(&self.library.roots());
        if self.nav.visible_page().is_none_or(|p| p != self.dupes.page) {
            self.nav.push(&self.dupes.page);
        }
    }

    fn set_enabled(&self, name: &str, enabled: bool) {
        if let Some(a) = self
            .win
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            a.set_enabled(enabled);
        }
    }

    /// Rotating and uploading only work with local pictures and not during a run.
    fn update_actions(&self) {
        let in_viewer = self
            .nav
            .visible_page()
            .is_some_and(|p| p == self.viewer.page);
        let targets = !self.target_paths(in_viewer).is_empty();
        let editable = !self.editable_photos(in_viewer).is_empty();
        let idle = !self.sheet.is_busy();
        self.set_enabled("rotate-left", idle && editable);
        self.set_enabled("rotate-right", idle && editable);
        self.set_enabled("upload", idle && targets && self.ctx.immich().is_some());
        self.set_enabled("trash", idle && targets);
        self.set_enabled("apply-look", idle && editable && !in_viewer);
        let remote = idle && self.ctx.immich().is_some() && !self.remote_ids(in_viewer).is_empty();
        self.set_enabled("download", remote);
        self.set_enabled("share-link", remote);
    }

    /// In the single view the current picture, otherwise the selection in the sheet.
    fn target_photos(&self, in_viewer: bool) -> Vec<Photo> {
        let photos = if in_viewer {
            self.viewer.current().into_iter().collect()
        } else {
            self.sheet.selected_photos()
        };
        photos.into_iter().filter(|p| p.path().is_some()).collect()
    }

    /// What can be edited (rotate, look): pictures only. Blick plays
    /// videos, nothing more.
    fn editable_photos(&self, in_viewer: bool) -> Vec<Photo> {
        self.target_photos(in_viewer)
            .into_iter()
            .filter(|p| !p.is_video())
            .collect()
    }

    /// Immich IDs of the selection (or of the picture shown).
    fn remote_ids(&self, in_viewer: bool) -> Vec<String> {
        let photos = if in_viewer {
            self.viewer.current().into_iter().collect()
        } else {
            self.sheet.selected_photos()
        };
        photos
            .iter()
            .filter_map(|p| p.remote_id().map(str::to_owned))
            .collect()
    }

    fn target_paths(&self, in_viewer: bool) -> Vec<PathBuf> {
        self.target_photos(in_viewer)
            .iter()
            .filter_map(|p| p.path().map(Path::to_owned))
            .collect()
    }

    fn in_viewer(&self) -> bool {
        self.nav
            .visible_page()
            .is_some_and(|p| p == self.viewer.page)
    }

    fn setup_signals(self: &Rc<Self>) {
        let w = self.weak();
        self.library.connect_open(glib::clone!(
            #[strong]
            w,
            move |dir| {
                if let Some(this) = w.upgrade() {
                    this.open_album(dir);
                }
            }
        ));
        self.library.connect_add(glib::clone!(
            #[strong]
            w,
            move || {
                if let Some(this) = w.upgrade() {
                    this.choose_library_folder();
                }
            }
        ));
        self.sheet.connect_activate(glib::clone!(
            #[strong]
            w,
            move |pos| {
                let Some(this) = w.upgrade() else { return };
                this.nav.push(&this.viewer.page);
                this.viewer.show(this.sheet.store.clone().upcast(), pos);
                this.update_actions();
            }
        ));
        self.sheet.connect_selection(glib::clone!(
            #[strong]
            w,
            move || {
                if let Some(this) = w.upgrade() {
                    this.update_actions();
                }
            }
        ));
        self.viewer.connect_files_changed(glib::clone!(
            #[strong]
            w,
            move || {
                // A new copy should show up in the sheet.
                if let Some(this) = w.upgrade()
                    && let Some(Origin::Folder(dir)) = this.sheet.origin()
                {
                    this.sheet.open_folder(dir);
                }
            }
        ));
        // In the single view every column counts for picture space: collapse the sidebar,
        // expand it again on the way back (if the window is wide enough).
        self.nav.connect_visible_page_notify(glib::clone!(
            #[strong]
            w,
            move |_| {
                if let Some(this) = w.upgrade() {
                    let viewer = this.in_viewer();
                    this.split.set_collapsed(viewer || this.win.width() < 600);
                    this.split.set_show_content(true);
                    if !viewer && this.win.is_fullscreen() {
                        this.win.unfullscreen();
                    }
                    if !viewer {
                        if this.sheet.origin().is_none() {
                            let dir = this
                                .direct
                                .borrow()
                                .as_ref()
                                .and_then(|s| s.item(0).and_downcast::<Photo>())
                                .and_then(|p| p.path().and_then(Path::parent).map(Path::to_owned));
                            if let Some(dir) = dir {
                                this.open_folder(dir);
                            }
                        } else {
                            this.sheet.focus_item(this.viewer.index());
                        }
                    }
                    this.update_actions();
                }
            }
        ));
        self.win.connect_fullscreened_notify(glib::clone!(
            #[strong]
            w,
            move |win| {
                if let Some(this) = w.upgrade() {
                    this.viewer.set_immersive(win.is_fullscreen());
                }
            }
        ));

        // Stick plugged in or out: refill the list, reset the selection to the open folder.
        let monitor = gio::VolumeMonitor::get();
        for signal in [0, 1] {
            let refresh = glib::clone!(
                #[strong]
                w,
                move |_: &gio::VolumeMonitor, _: &gio::Mount| {
                    let Some(this) = w.upgrade() else { return };
                    let custom = this.custom_place.borrow().clone();
                    this.fill_places(custom.as_deref());
                    if let Some(Origin::Folder(dir)) = this.sheet.origin() {
                        let idx = this
                            .place_paths
                            .borrow()
                            .iter()
                            .position(|p| p.as_deref() == Some(dir.as_path()));
                        if let Some(row) = idx.and_then(|i| this.places.row_at_index(i as i32)) {
                            this.places.select_row(Some(&row));
                        }
                    }
                }
            );
            if signal == 0 {
                monitor.connect_mount_added(refresh);
            } else {
                monitor.connect_mount_removed(refresh);
            }
        }

        self.places.connect_row_activated(glib::clone!(
            #[strong]
            w,
            move |_, row| {
                let Some(this) = w.upgrade() else { return };
                let path = this
                    .place_paths
                    .borrow()
                    .get(row.index() as usize)
                    .cloned()
                    .flatten();
                match path {
                    Some(p) => this.open_folder(p),
                    None => this.choose_folder(),
                }
            }
        ));
        self.albums.connect_row_activated(glib::clone!(
            #[strong]
            w,
            move |_, row| {
                let Some(this) = w.upgrade() else { return };
                if this.ctx.immich().is_none() {
                    this.preferences();
                    return;
                }
                let entry = this.album_ids.borrow().get(row.index() as usize).cloned();
                if let Some((id, name)) = entry {
                    this.places.unselect_all();
                    this.library.unselect();
                    this.nav.pop_to_tag("sheet");
                    this.sheet.open_immich(id, name);
                    this.split.set_show_content(true);
                }
            }
        ));

        self.win.connect_close_request(glib::clone!(
            #[strong]
            w,
            move |win| {
                if let Some(this) = w.upgrade() {
                    let (width, height) = win.default_size();
                    let _ = this.settings.set_int("window-width", width);
                    let _ = this.settings.set_int("window-height", height);
                    let _ = this
                        .settings
                        .set_boolean("window-maximized", win.is_maximized());
                }
                glib::Propagation::Proceed
            }
        ));
    }

    fn fill_places(&self, custom: Option<&Path>) {
        self.custom_place.replace(custom.map(Path::to_owned));
        while let Some(row) = self.places.row_at_index(0) {
            self.places.remove(&row);
        }
        let mut paths = Vec::new();
        let pictures = glib::user_special_dir(glib::UserDirectory::Pictures);
        if let Some(p) = &pictures {
            self.places
                .append(&sidebar_row("folder-pictures-symbolic", "Pictures", None));
            paths.push(Some(p.clone()));
        }
        if let Some(c) = custom.filter(|c| Some(*c) != pictures.as_deref()) {
            let name = c
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| c.display().to_string());
            let row = sidebar_row("folder-symbolic", &name, None);
            row.set_tooltip_text(Some(&c.display().to_string()));
            self.places.append(&row);
            paths.push(Some(c.to_owned()));
        }
        for (name, path) in removable_mounts() {
            if Some(&path) == custom.map(Path::to_owned).as_ref() {
                continue;
            }
            let row = sidebar_row("media-removable-symbolic", &name, None);
            row.set_tooltip_text(Some(&path.display().to_string()));
            self.places.append(&row);
            paths.push(Some(path));
        }
        self.places.append(&sidebar_row(
            "folder-open-symbolic",
            "Open folder …",
            None,
        ));
        paths.push(None);
        self.place_paths.replace(paths);
    }

    fn open_folder(&self, dir: PathBuf) {
        self.fill_places(Some(&dir));
        let idx = self
            .place_paths
            .borrow()
            .iter()
            .position(|p| p.as_deref() == Some(dir.as_path()));
        if let Some(row) = idx.and_then(|i| self.places.row_at_index(i as i32)) {
            self.places.select_row(Some(&row));
        }
        self.albums.unselect_all();
        self.library.unselect();
        self.show_folder(dir);
    }

    /// Album from the library: same view, but without an entry under "On this device".
    fn open_album(&self, dir: PathBuf) {
        self.places.unselect_all();
        self.albums.unselect_all();
        self.show_folder(dir);
    }

    fn show_folder(&self, dir: PathBuf) {
        let _ = self
            .settings
            .set_string("last-folder", &dir.to_string_lossy());
        self.nav.pop_to_tag("sheet");
        self.sheet.open_folder(dir);
        self.split.set_show_content(true);
    }

    fn import_look_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Import look folder")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Ok(Some(path)) = dialog
                    .select_folder_future(Some(&this.win))
                    .await
                    .map(|f| f.path())
                {
                    this.viewer.run_import(vec![path]).await;
                }
            }
        ));
    }

    /// Downloads RawTherapee's Film Simulation Collection (CC BY-SA 4.0).
    /// Progress runs in the toast; the whole thing takes a few minutes depending on the line.
    fn download_looks(self: &Rc<Self>) {
        let toast = adw::Toast::builder()
            .title("Downloading film simulations …")
            .timeout(0)
            .build();
        self.toasts.add_toast(toast.clone());
        let (tx, rx) = async_channel::unbounded::<(u64, u64)>();
        let task =
            crate::runtime::spawn_handle(crate::looks::download_rawtherapee(move |done, total| {
                let _ = tx.try_send((done, total));
            }));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut last = 0;
                while let Ok((done, total)) = rx.recv().await {
                    let mb = done / 1_000_000;
                    if mb != last {
                        last = mb;
                        toast.set_title(&format!(
                            "Downloading film simulations … {mb} of {} MB",
                            total / 1_000_000
                        ));
                    }
                }
                toast.dismiss();
                let text = match task.await {
                    Ok(Ok(n)) => {
                        this.viewer.reload_looks();
                        format!("{n} RawTherapee film simulations downloaded")
                    }
                    Ok(Err(e)) => format!("Download failed: {e:#}"),
                    Err(e) => format!("Download cancelled: {e}"),
                };
                this.toasts.add_toast(adw::Toast::new(&text));
            }
        ));
    }

    fn choose_library_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Add folder to library")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Ok(Some(path)) = dialog
                    .select_folder_future(Some(&this.win))
                    .await
                    .map(|f| f.path())
                {
                    this.library.add_root(path);
                }
            }
        ));
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Open folder")
            .modal(true)
            .build();
        if let Some(Origin::Folder(dir)) = self.sheet.origin() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                // Cancel returns an error, which is no reason for a message.
                if let Ok(Some(path)) = dialog
                    .select_folder_future(Some(&this.win))
                    .await
                    .map(|f| f.path())
                {
                    // Opened folders become part of the library together with their subfolders.
                    this.library.add_root(path.clone());
                    this.open_folder(path);
                }
            }
        ));
    }

    fn rotate(self: &Rc<Self>, rot: Rotation) {
        if self.in_viewer() {
            self.viewer.rotate(rot);
            return;
        }
        let photos = self.editable_photos(false);
        self.rotate_photos(photos, rot, true);
    }

    fn rotate_photos(self: &Rc<Self>, photos: Vec<Photo>, rot: Rotation, offer_undo: bool) {
        self.run_batch(
            photos,
            "Rotating",
            move |p| blick_core::rotate::rotate_file(p, rot).map(|_| ()),
            move |this, photos, report| {
                let done: Vec<Photo> = photos
                    .iter()
                    .filter(|p| p.path().is_some_and(|p| report.done.iter().any(|d| d == p)))
                    .cloned()
                    .collect();
                for photo in &done {
                    if let Some(path) = photo.path() {
                        this.ctx.previews.remove_where(|k| k.0 == path);
                    }
                    photo.reload(&this.ctx);
                }
                let toast = adw::Toast::builder()
                    .title(report_text("rotated", &report))
                    .build();
                if offer_undo && !done.is_empty() {
                    toast.set_button_label(Some("Undo"));
                    let w = Rc::downgrade(this);
                    toast.connect_button_clicked(move |_| {
                        if let Some(this) = w.upgrade() {
                            this.rotate_photos(done.clone(), rot.inverse(), false);
                        }
                    });
                }
                this.toasts.add_toast(toast);
            },
        );
    }

    /// Batch over local pictures with progress in the contact sheet.
    fn run_batch<F, D>(self: &Rc<Self>, photos: Vec<Photo>, verb: &'static str, op: F, done: D)
    where
        F: Fn(&Path) -> blick_core::Result<()> + Send + Sync + 'static,
        D: FnOnce(&Rc<Self>, &[Photo], blick_core::batch::Report) + 'static,
    {
        let photos: Vec<Photo> = photos.into_iter().filter(|p| p.path().is_some()).collect();
        let paths: Vec<PathBuf> = photos
            .iter()
            .filter_map(|p| p.path().map(Path::to_owned))
            .collect();
        if paths.is_empty() {
            return;
        }
        let total = paths.len();
        let cancel = Arc::new(AtomicBool::new(false));
        self.sheet.set_busy(cancel.clone());
        self.sheet.set_progress(verb, 0, total);
        self.update_actions();

        let (tx, rx) = async_channel::unbounded::<usize>();
        let job = gio::spawn_blocking(move || {
            blick_core::batch::run(
                &paths,
                &cancel,
                |n, _| {
                    let _ = tx.send_blocking(n);
                },
                op,
            )
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(n) = rx.recv().await {
                    this.sheet.set_progress(verb, n, total);
                }
                let report = job.await;
                this.sheet.set_idle();
                this.update_actions();
                match report {
                    Ok(report) => {
                        for (path, e) in &report.failed {
                            tracing::warn!("{verb} {}: {e}", path.display());
                        }
                        done(&this, &photos, report);
                    }
                    Err(_) => this
                        .toasts
                        .add_toast(adw::Toast::new("The operation crashed.")),
                }
            }
        ));
    }

    /// Apply a look to the selection. This creates copies, the originals stay.
    fn apply_look(self: &Rc<Self>) {
        let photos = self.editable_photos(false);
        if photos.is_empty() {
            return;
        }
        let w = Rc::downgrade(self);
        crate::looks::pick(&self.win, self.looks.clone(), None, move |item| {
            let (Some(this), Some(item)) = (w.upgrade(), item) else {
                return;
            };
            let photos = photos.clone();
            glib::spawn_future_local(async move {
                let look = match crate::looks::load(&item).await {
                    Ok(l) => l,
                    Err(e) => {
                        this.toasts.add_toast(adw::Toast::new(&format!(
                            "Can't load look: {e:#}"
                        )));
                        return;
                    }
                };
                let name = look.name.clone();
                let render = Arc::new(look.render(1.0));
                this.run_batch(
                    photos,
                    "Applying look:",
                    move |p| {
                        let dst = blick_core::export::copy_path(p);
                        blick_core::export::save_adjusted(
                            p,
                            &dst,
                            None,
                            blick_core::Adjustments::default(),
                            Some(&render),
                        )
                    },
                    move |this, _, report| {
                        let text = format!(
                            "{}, look {name}",
                            report_text("saved as copy", &report)
                        );
                        this.toasts.add_toast(adw::Toast::new(&text));
                        this.sheet.refresh();
                    },
                );
            });
        });
    }

    /// To the trash, never permanently. The toast offers Undo.
    fn trash(self: &Rc<Self>) {
        let in_viewer = self.in_viewer();
        let photos = self.target_photos(in_viewer);
        if photos.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut trashed = Vec::new();
                let mut failed = None;
                for photo in &photos {
                    let Some(path) = photo.path().map(Path::to_owned) else {
                        continue;
                    };
                    match gio::File::for_path(&path)
                        .trash_future(glib::Priority::DEFAULT)
                        .await
                    {
                        Ok(()) => trashed.push(path),
                        Err(e) => failed = Some(e.to_string()),
                    }
                }
                this.sheet.remove_paths(&trashed);
                if let Some(store) = this.direct.borrow().as_ref() {
                    for i in (0..store.n_items()).rev() {
                        let gone = store
                            .item(i)
                            .and_downcast::<Photo>()
                            .is_some_and(|p| p.path().is_some_and(|x| trashed.iter().any(|t| t == x)));
                        if gone {
                            store.remove(i);
                        }
                    }
                }
                if in_viewer {
                    this.viewer.after_removal();
                }
                let mut text = format!("{} moved to the trash", picture_count(trashed.len()));
                if let Some(e) = failed {
                    text.push_str(&format!(". Not all: {e}"));
                }
                let toast = adw::Toast::builder().title(text).timeout(8).build();
                if !trashed.is_empty() {
                    toast.set_button_label(Some("Undo"));
                    let w = Rc::downgrade(&this);
                    toast.connect_button_clicked(move |_| {
                        if let Some(this) = w.upgrade() {
                            this.restore(trashed.clone());
                        }
                    });
                }
                this.toasts.add_toast(toast);
            }
        ));
    }

    /// Brings files back from the trash to their old place.
    fn restore(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let job = gio::spawn_blocking(move || -> Result<usize, String> {
            let trash = gio::File::for_uri("trash:///");
            let children = trash
                .enumerate_children(
                    "standard::name,trash::orig-path",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .map_err(|e| e.to_string())?;
            let mut n = 0;
            for info in children.flatten() {
                let orig = info
                    .attribute_byte_string("trash::orig-path")
                    .map(PathBuf::from);
                if let Some(orig) = orig.filter(|o| paths.contains(o)) {
                    let src = trash.child(info.name());
                    src.move_(
                        &gio::File::for_path(&orig),
                        gio::FileCopyFlags::NONE,
                        gio::Cancellable::NONE,
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                    n += 1;
                }
            }
            Ok(n)
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let text = match job.await {
                    Ok(Ok(n)) if n > 0 => format!("{} restored", picture_count(n)),
                    Ok(Ok(_)) => "Not found in the trash. Restore it from the trash in Files.".into(),
                    Ok(Err(e)) => format!("Restoring failed ({e}). Restore it from the trash in Files."),
                    Err(_) => "Restoring crashed.".into(),
                };
                this.sheet.refresh();
                this.toasts.add_toast(adw::Toast::new(&text));
            }
        ));
    }

    fn upload(self: &Rc<Self>) {
        let Some(client) = self.ctx.immich() else {
            return;
        };
        let paths = self.target_paths(self.in_viewer());
        if paths.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                this.nav.pop_to_tag("sheet");
                crate::upload::run(&this.sheet, client.clone(), paths, &this.toasts).await;
                // Refresh the album counter in the sidebar.
                this.set_client(Some(client));
            }
        ));
    }

    /// Save originals from Immich. One through the save dialog with its
    /// file name, several into a chosen folder, with progress in the sheet.
    fn download(self: &Rc<Self>) {
        let Some(client) = self.ctx.immich() else {
            return;
        };
        let ids = self.remote_ids(self.in_viewer());
        if ids.is_empty() {
            return;
        }
        let pictures =
            glib::user_special_dir(glib::UserDirectory::Pictures).map(gio::File::for_path);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let [id] = ids.as_slice() {
                    let id = id.clone();
                    let name = {
                        let (c, i) = (client.clone(), id.clone());
                        runtime::spawn(async move { Ok(c.original_file_name(&i).await?) })
                            .await
                            .unwrap_or_else(|_| format!("{id}.jpg"))
                    };
                    let dialog = gtk::FileDialog::builder()
                        .title("Save original")
                        .initial_name(name.as_str())
                        .modal(true)
                        .build();
                    dialog.set_initial_folder(pictures.as_ref());
                    let Ok(Some(dst)) = dialog.save_future(Some(&this.win)).await.map(|f| f.path())
                    else {
                        return;
                    };
                    this.toasts
                        .add_toast(adw::Toast::new(&format!("Downloading {name} …")));
                    let r =
                        runtime::spawn(
                            async move { Ok(client.download_original(&id, &dst).await?) },
                        )
                        .await;
                    let text = match r {
                        Ok(_) => format!("{name} saved"),
                        Err(e) => format!("Download failed: {e:#}"),
                    };
                    this.toasts.add_toast(adw::Toast::new(&text));
                    return;
                }

                let dialog = gtk::FileDialog::builder()
                    .title(format!("Save {} to …", picture_count(ids.len())))
                    .accept_label("Save")
                    .modal(true)
                    .build();
                dialog.set_initial_folder(pictures.as_ref());
                let Ok(Some(dir)) = dialog
                    .select_folder_future(Some(&this.win))
                    .await
                    .map(|f| f.path())
                else {
                    return;
                };
                let total = ids.len();
                let cancel = Arc::new(AtomicBool::new(false));
                this.nav.pop_to_tag("sheet");
                this.sheet.set_busy(cancel.clone());
                this.sheet.set_progress("Downloading", 0, total);
                this.update_actions();
                let (mut ok, mut failed) = (0usize, 0usize);
                for (n, id) in ids.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let (c, d) = (client.clone(), dir.clone());
                    let r = runtime::spawn(async move {
                        let name = c.original_file_name(&id).await?;
                        let dst = free_name(&d, &name);
                        c.download_original(&id, &dst).await?;
                        Ok(())
                    })
                    .await;
                    match r {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            failed += 1;
                            tracing::warn!("download: {e:#}");
                        }
                    }
                    this.sheet.set_progress("Downloading", n + 1, total);
                }
                this.sheet.set_idle();
                this.update_actions();
                let mut text = format!("{} saved", picture_count(ok));
                if failed > 0 {
                    text.push_str(&format!(", {failed} failed"));
                }
                this.toasts.add_toast(adw::Toast::new(&text));
            }
        ));
    }

    /// Create a share link in Immich and put it on the clipboard.
    fn share_link(self: &Rc<Self>) {
        let Some(client) = self.ctx.immich() else {
            return;
        };
        let ids = self.remote_ids(self.in_viewer());
        if ids.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let n = ids.len();
                match runtime::spawn(async move { Ok(client.share_link(&ids).await?) }).await {
                    Ok(url) => {
                        this.win.clipboard().set_text(&url);
                        this.toasts
                            .add_toast(adw::Toast::new(&format!("Link for {} copied", picture_count(n))));
                    }
                    Err(e) => this
                        .toasts
                        .add_toast(adw::Toast::new(&format!("Link not created: {e:#}"))),
                }
            }
        ));
    }

    fn preferences(self: &Rc<Self>) {
        let w = self.weak();
        crate::prefs::present(&self.win, &self.settings, move |client| {
            if let Some(this) = w.upgrade() {
                this.set_client(Some(client));
            }
        });
    }

    fn about(&self) {
        // Release notes come from the metainfo, which is built in as a resource.
        let about = adw::AboutDialog::from_appdata(
            "/io/github/paulhoever/Blick/metainfo.xml",
            Some(env!("CARGO_PKG_VERSION")),
        );
        about.set_version(env!("CARGO_PKG_VERSION"));
        about.set_comments("View, rotate and adjust pictures and upload them to Immich.");
        about.add_link("Source code", "https://github.com/paulhoever/Blick");
        about.present(Some(&self.win));
    }

    /// On start: environment variables or the address from GSettings plus the key from the keyring.
    fn connect_immich(self: &Rc<Self>) {
        if let Some((url, key)) = config::env_override() {
            match blick_immich::Client::new(&url, &key) {
                Ok(c) => self.set_client(Some(c)),
                Err(e) => self.toasts.add_toast(adw::Toast::new(&e.to_string())),
            }
            return;
        }
        let url = self.settings.string("immich-url").to_string();
        if url.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match runtime::spawn(config::load_api_key(url.clone())).await {
                    Ok(Some(key)) => match blick_immich::Client::new(&url, &key) {
                        Ok(c) => this.set_client(Some(c)),
                        Err(e) => tracing::warn!("{e}"),
                    },
                    Ok(None) => {}
                    Err(e) => tracing::warn!("keyring: {e:#}"),
                }
            }
        ));
    }

    fn set_client(self: &Rc<Self>, client: Option<blick_immich::Client>) {
        self.ctx.immich.replace(client.clone());
        self.update_actions();
        let Some(client) = client else {
            self.render_albums(None);
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match runtime::spawn(async move { Ok(client.albums().await?) }).await {
                    Ok(albums) => this.render_albums(Some(Ok(albums))),
                    Err(e) => this.render_albums(Some(Err(e))),
                }
            }
        ));
    }

    fn render_albums(&self, albums: Option<anyhow::Result<Vec<blick_immich::Album>>>) {
        while let Some(row) = self.albums.row_at_index(0) {
            self.albums.remove(&row);
        }
        let mut ids = Vec::new();
        match albums {
            None => {
                self.albums
                    .append(&sidebar_row("network-server-symbolic", "Connect …", None));
            }
            Some(Err(e)) => {
                let row = sidebar_row("dialog-warning-symbolic", "Unreachable", None);
                row.set_tooltip_text(Some(&format!("{e:#}")));
                self.albums.append(&row);
                self.toasts
                    .add_toast(adw::Toast::new(&format!("Immich: {e:#}")));
            }
            Some(Ok(albums)) => {
                self.albums
                    .append(&sidebar_row("image-x-generic-symbolic", "All photos", None));
                ids.push((None, "All photos".to_owned()));
                for a in albums {
                    let icon = if a.shared {
                        "folder-publicshare-symbolic"
                    } else {
                        "folder-pictures-symbolic"
                    };
                    self.albums.append(&sidebar_row(
                        icon,
                        &a.album_name,
                        Some(&a.asset_count.to_string()),
                    ));
                    ids.push((Some(a.id), a.album_name));
                }
            }
        }
        self.album_ids.replace(ids);
    }
}

/// `name` in the folder, on a collision with " (2)", " (3)" … before the extension.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|c| !c.exists())
        .expect("unendlich viele Kandidaten")
}

fn report_text(verb: &str, report: &blick_core::batch::Report) -> String {
    let mut text = format!("{} {verb}", picture_count(report.done.len()));
    if let Some((_, e)) = report.failed.first() {
        text.push_str(&format!(", {} failed: {e}", report.failed.len()));
    }
    if report.cancelled > 0 {
        text.push_str(&format!(", {} cancelled", report.cancelled));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::free_name;

    #[test]
    fn free_name_counts_up_before_extension() {
        let dir = std::env::temp_dir().join(format!("blick-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "IMG_1.HEIC"), dir.join("IMG_1.HEIC"));
        std::fs::write(dir.join("IMG_1.HEIC"), b"").unwrap();
        assert_eq!(free_name(&dir, "IMG_1.HEIC"), dir.join("IMG_1 (2).HEIC"));
        std::fs::write(dir.join("IMG_1 (2).HEIC"), b"").unwrap();
        assert_eq!(free_name(&dir, "IMG_1.HEIC"), dir.join("IMG_1 (3).HEIC"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
