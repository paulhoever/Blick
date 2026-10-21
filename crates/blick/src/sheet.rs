//! Der Kontaktbogen: Raster aller Bilder eines Ordners oder Immich-Albums.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use blick_immich::TimeBucket;
use gtk::{gio, glib};

use crate::ctx::Ctx;
use crate::photo::Photo;
use crate::runtime;
use crate::tile::Tile;

#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Folder(PathBuf),
    /// `id: None` ist die ganze Immich-Zeitleiste.
    Immich {
        id: Option<String>,
        name: String,
    },
}

type Callback<T> = Box<dyn Fn(T)>;

pub struct Sheet {
    pub page: adw::NavigationPage,
    /// „Mit Unterordnern“: der Bogen zeigt den ganzen Baum. Das Fenster bindet
    /// den Schalter an GSettings.
    pub subfolders: gtk::ToggleButton,
    /// Beschriftete Knöpfe der Auswahlleiste, für den schmalen Breakpoint.
    pub labeled_buttons: [adw::ButtonContent; 2],
    scan_cancel: RefCell<Arc<AtomicBool>>,
    grid: gtk::GridView,
    pub store: gio::ListStore,
    pub selection: gtk::MultiSelection,
    ctx: Rc<Ctx>,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    bar: gtk::ActionBar,
    count: gtk::Label,
    actions: gtk::Box,
    busy: gtk::Box,
    progress: gtk::ProgressBar,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    origin: RefCell<Option<Origin>>,
    /// Zählt bei jedem Wechsel hoch; späte Ergebnisse alter Ordner werden verworfen.
    generation: Cell<u64>,
    pending_buckets: RefCell<VecDeque<TimeBucket>>,
    loading_more: Cell<bool>,
    /// Alle Monate der Zeitleiste, neueste zuerst; dazu, ab welcher Stelle im
    /// Bogen jeder schon geladene Monat beginnt (gleicher Index).
    buckets: RefCell<Vec<TimeBucket>>,
    bucket_starts: RefCell<Vec<u32>>,
    /// Sprungziel aus der Monatsliste, solange die Monate davor noch laden.
    jump_target: Cell<Option<usize>>,
    jump: gtk::MenuButton,
    jump_list: gtk::ListBox,
    /// Monat zu jeder Zeile der Sprungliste; Jahresüberschriften haben keinen.
    jump_rows: RefCell<Vec<Option<usize>>>,
    month_label: gtk::Label,
    month_reveal: gtk::Revealer,
    month_hide: RefCell<Option<glib::SourceId>>,
    context_menu: gtk::PopoverMenu,
    on_activate: RefCell<Option<Callback<u32>>>,
    on_selection: RefCell<Option<Callback<()>>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    refresh_pending: Cell<bool>,
    rewatch: Cell<bool>,
    /// Pfade, deren Inhalt sich laut Monitor geändert hat; beim nächsten Abgleich neu laden.
    changed: RefCell<Vec<PathBuf>>,
}

const MONATE: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];

/// „Oktober 2026“; ohne lesbares Datum der Rohwert.
fn month_title(b: &TimeBucket) -> String {
    match b.year_month() {
        Some((y, m)) => format!("{} {y}", MONATE[m as usize - 1]),
        None => b.time_bucket.clone(),
    }
}

pub fn bilder(n: usize) -> String {
    if n == 1 {
        "1 Bild".into()
    } else {
        format!("{n} Bilder")
    }
}

impl Sheet {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let store = gio::ListStore::new::<Photo>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
            item.set_child(Some(&Tile::default()));
        });
        factory.connect_bind(glib::clone!(
            #[strong]
            ctx,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let (Some(photo), Some(tile)) = (
                    item.item().and_downcast::<Photo>(),
                    item.child().and_downcast::<Tile>(),
                ) else {
                    return;
                };
                tile.bind(&photo);
                photo.ensure_thumbnail(&ctx);
            }
        ));
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
            if let Some(tile) = item.child().and_downcast::<Tile>() {
                tile.unbind();
            }
            if let Some(photo) = item.item().and_downcast::<Photo>() {
                photo.cancel_thumbnail();
            }
        });

        let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
        grid.set_max_columns(24);
        grid.set_enable_rubberband(true);

        let scroller = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .build();
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        // Beim Scrollen durch Immich zeigt ein Schild oben, welcher Monat gerade
        // zu sehen ist, wie in Immich und GNOME Fotos.
        let month_label = gtk::Label::builder()
            .css_classes(["osd", "monthpill", "heading"])
            .build();
        let month_reveal = gtk::Revealer::builder()
            .child(&month_label)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(12)
            .can_target(false)
            .build();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroller));
        overlay.add_overlay(&month_reveal);

        let menu = gio::Menu::new();
        menu.append(Some("Herunterladen …"), Some("win.download"));
        menu.append(Some("Freigabelink kopieren"), Some("win.share-link"));
        let context_menu = gtk::PopoverMenu::from_model(Some(&menu));
        context_menu.set_has_arrow(false);
        context_menu.set_halign(gtk::Align::Start);
        context_menu.set_parent(&overlay);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&overlay, Some("grid"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&spinner, Some("loading"));

        let count = gtk::Label::new(None);
        count.set_css_classes(&["numeric", "dim-label"]);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let rotate_left = gtk::Button::builder()
            .icon_name("object-rotate-left-symbolic")
            .tooltip_text("Nach links drehen (Strg+Umschalt+R)")
            .action_name("win.rotate-left")
            .build();
        let rotate_right = gtk::Button::builder()
            .icon_name("object-rotate-right-symbolic")
            .tooltip_text("Nach rechts drehen (Strg+R)")
            .action_name("win.rotate-right")
            .build();
        // Knöpfe mit Symbol und Text wie die Auswahlleiste im Mockup von GNOME
        // Fotos. Wird das Fenster schmal, blendet ein Breakpoint die Texte aus.
        let upload_content = adw::ButtonContent::builder()
            .icon_name("send-to-symbolic")
            .label("Zu Immich hochladen")
            .can_shrink(true)
            .build();
        let upload = gtk::Button::builder()
            .child(&upload_content)
            .tooltip_text("Zu Immich hochladen (Strg+U)")
            .action_name("win.upload")
            .build();
        let look_content = adw::ButtonContent::builder()
            .icon_name("preferences-color-symbolic")
            .label("Stil anwenden …")
            .can_shrink(true)
            .build();
        let apply_look = gtk::Button::builder()
            .child(&look_content)
            .tooltip_text("Erzeugt Kopien im gewählten Stil")
            .action_name("win.apply-look")
            .build();
        for b in [&upload, &apply_look] {
            b.update_property(&[gtk::accessible::Property::Label(
                &b.tooltip_text().unwrap_or_default(),
            )]);
        }
        actions.append(&rotate_left);
        actions.append(&rotate_right);
        actions.append(&apply_look);
        actions.append(&upload);

        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .hexpand(true)
            .show_text(true)
            .build();
        let cancel_btn = gtk::Button::with_label("Abbrechen");
        let busy = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        busy.set_hexpand(true);
        busy.append(&progress);
        busy.append(&cancel_btn);
        busy.set_visible(false);

        let bar = gtk::ActionBar::new();
        bar.pack_start(&count);
        bar.set_center_widget(Some(&busy));
        bar.pack_end(&actions);
        bar.set_revealed(false);

        let title = adw::WindowTitle::new("Blick", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let subfolders = gtk::ToggleButton::builder()
            .icon_name("folder-symbolic")
            .tooltip_text("Mit Unterordnern")
            .active(true)
            .build();
        subfolders.update_property(&[gtk::accessible::Property::Label(
            "Bilder aus Unterordnern zeigen",
        )]);
        header.pack_end(&subfolders);
        let jump_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let jump_scroll = gtk::ScrolledWindow::builder()
            .child(&jump_list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(480)
            .min_content_width(220)
            .build();
        let jump_popover = gtk::Popover::builder().child(&jump_scroll).build();
        let jump = gtk::MenuButton::builder()
            .icon_name("x-office-calendar-symbolic")
            .tooltip_text("Zu Monat springen")
            .popover(&jump_popover)
            .visible(false)
            .build();
        jump.update_property(&[gtk::accessible::Property::Label("Zu Monat springen")]);
        header.pack_end(&jump);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));
        view.add_bottom_bar(&bar);

        let page = adw::NavigationPage::builder()
            .title("Kontaktbogen")
            .tag("sheet")
            .child(&view)
            .build();

        let this = Rc::new(Self {
            page,
            subfolders,
            labeled_buttons: [upload_content, look_content],
            scan_cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
            grid: grid.clone(),
            store,
            selection,
            ctx,
            title,
            stack,
            empty,
            scroller,
            bar,
            count,
            actions,
            busy,
            progress,
            cancel: RefCell::new(None),
            origin: RefCell::new(None),
            generation: Cell::new(0),
            pending_buckets: RefCell::new(VecDeque::new()),
            loading_more: Cell::new(false),
            buckets: RefCell::new(Vec::new()),
            bucket_starts: RefCell::new(Vec::new()),
            jump_target: Cell::new(None),
            jump,
            jump_list,
            jump_rows: RefCell::new(Vec::new()),
            month_label,
            month_reveal,
            month_hide: RefCell::new(None),
            context_menu,
            on_activate: RefCell::new(None),
            on_selection: RefCell::new(None),
            monitors: RefCell::new(Vec::new()),
            refresh_pending: Cell::new(false),
            rewatch: Cell::new(false),
            changed: RefCell::new(Vec::new()),
        });

        grid.connect_activate(glib::clone!(
            #[weak]
            this,
            move |_, pos| {
                if let Some(f) = this.on_activate.borrow().as_ref() {
                    f(pos);
                }
            }
        ));
        this.subfolders.connect_toggled(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(Origin::Folder(dir)) = this.origin() {
                    this.open_folder(dir);
                }
            }
        ));

        // Leertaste öffnet das Bild unter dem Fokus, wie ein Schnellblick.
        // Capture, weil die GridView die Leertaste sonst für die Auswahl nimmt.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| {
                if key != gtk::gdk::Key::space || !mods.is_empty() {
                    return glib::Propagation::Proceed;
                }
                let focused = this
                    .grid
                    .focus_child()
                    .and_then(|item| item.first_child())
                    .and_downcast::<Tile>()
                    .map(|t| t.position());
                let selected = this.selection.selection();
                let pos = focused.or_else(|| (!selected.is_empty()).then(|| selected.nth(0)));
                match (pos, this.on_activate.borrow().as_ref()) {
                    (Some(pos), Some(f)) if pos < this.store.n_items() => {
                        f(pos);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        this.grid.add_controller(keys);

        this.selection.connect_selection_changed(glib::clone!(
            #[weak]
            this,
            move |_, _, _| this.selection_changed()
        ));
        cancel_btn.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(c) = this.cancel.borrow().as_ref() {
                    c.store(true, Ordering::Relaxed);
                }
            }
        ));
        // Immich-Alben kommen monatsweise; kurz vor dem Ende den nächsten Monat holen.
        this.scroller
            .vadjustment()
            .connect_value_changed(glib::clone!(
                #[weak]
                this,
                move |adj| {
                    if adj.value() + adj.page_size() * 3.0 > adj.upper() {
                        this.load_next_bucket();
                    }
                    this.show_month();
                }
            ));

        // Rechtsklick (oder langes Drücken) auf ein Bild: Menü für Immich-Bilder.
        // Capture, weil die Listeneinträge Klicks sonst selbst nehmen.
        let right = gtk::GestureClick::builder()
            .button(3)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        right.connect_pressed(glib::clone!(
            #[weak]
            this,
            move |g, _, x, y| {
                if this.popup_menu(x, y) {
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        ));
        this.grid.add_controller(right);
        let long = gtk::GestureLongPress::new();
        long.set_touch_only(true);
        long.set_propagation_phase(gtk::PropagationPhase::Capture);
        long.connect_pressed(glib::clone!(
            #[weak]
            this,
            move |g, x, y| {
                if this.popup_menu(x, y) {
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        ));
        this.grid.add_controller(long);
        this.jump_list.connect_row_activated(glib::clone!(
            #[weak]
            this,
            move |_, row| {
                let target = usize::try_from(row.index())
                    .ok()
                    .and_then(|r| this.jump_rows.borrow().get(r).copied().flatten());
                if let Some(i) = target {
                    this.jump.popdown();
                    this.jump_to_bucket(i);
                }
            }
        ));
        this
    }

    /// Bogenposition des Bildes unter dem Punkt (Koordinaten der GridView).
    fn position_at(&self, x: f64, y: f64) -> Option<u32> {
        self.grid
            .pick(x, y, gtk::PickFlags::DEFAULT)?
            .ancestor(Tile::static_type())
            .and_downcast::<Tile>()
            .map(|t| t.position())
            .filter(|&p| p < self.store.n_items())
    }

    /// Menü an der Stelle öffnen. Ein Bild außerhalb der Auswahl wird zur
    /// alleinigen Auswahl, wie in Nautilus.
    fn popup_menu(&self, x: f64, y: f64) -> bool {
        if !matches!(self.origin(), Some(Origin::Immich { .. })) {
            return false;
        }
        let Some(pos) = self.position_at(x, y) else {
            return false;
        };
        if !self.selection.is_selected(pos) {
            self.selection.select_item(pos, true);
        }
        let Some(parent) = self.context_menu.parent() else {
            return false;
        };
        let p = self
            .grid
            .compute_point(&parent, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        self.context_menu
            .set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                p.x() as i32,
                p.y() as i32,
                1,
                1,
            )));
        self.context_menu.popup();
        true
    }

    /// Index des Monats, in dem die Bogenposition liegt.
    fn bucket_of(&self, pos: u32) -> Option<usize> {
        let starts = self.bucket_starts.borrow();
        starts.partition_point(|&s| s <= pos).checked_sub(1)
    }

    /// Monatsschild für die oberste sichtbare Reihe; blendet sich nach einer
    /// Pause wieder aus.
    fn show_month(self: &Rc<Self>) {
        if self.buckets.borrow().is_empty() {
            return;
        }
        let w = self.grid.width() as f64;
        // Von rechts: Beginnt ein Monat mitten in der Reihe, gehört das rechte
        // Bild zu dem Monat, der darunter weitergeht. Mehrere Punkte, falls einer
        // in eine Lücke fällt.
        let pos = [0.9, 0.7, 0.5, 0.3, 0.1]
            .iter()
            .find_map(|f| self.position_at(w * f, 24.0));
        let Some(i) = pos.and_then(|p| self.bucket_of(p)) else {
            return;
        };
        let Some(title) = self.buckets.borrow().get(i).map(month_title) else {
            return;
        };
        self.month_label.set_label(&title);
        self.month_reveal.set_reveal_child(true);
        if let Some(id) = self.month_hide.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(1500),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.month_hide.take();
                    this.month_reveal.set_reveal_child(false);
                }
            ),
        );
        self.month_hide.replace(Some(id));
    }

    /// Monatsliste im Sprungmenü: Jahre als Überschrift, darunter die Monate.
    fn fill_jump_list(self: &Rc<Self>) {
        self.jump_list.remove_all();
        let mut rows = Vec::new();
        let mut year = None;
        for (i, b) in self.buckets.borrow().iter().enumerate() {
            let (y, m) = b.year_month().unwrap_or((0, 0));
            if year != Some(y) {
                year = Some(y);
                let label = gtk::Label::builder()
                    .label(if y == 0 {
                        "Ohne Datum".to_owned()
                    } else {
                        y.to_string()
                    })
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .margin_top(6)
                    .build();
                let row = gtk::ListBoxRow::builder()
                    .child(&label)
                    .activatable(false)
                    .selectable(false)
                    .build();
                self.jump_list.append(&row);
                rows.push(None);
            }
            let name = gtk::Label::builder()
                .label(if m == 0 {
                    b.time_bucket.as_str()
                } else {
                    MONATE[m as usize - 1]
                })
                .xalign(0.0)
                .hexpand(true)
                .build();
            let count = gtk::Label::builder()
                .label(b.count.to_string())
                .css_classes(["dim-label", "numeric"])
                .build();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            line.append(&name);
            line.append(&count);
            let row = gtk::ListBoxRow::builder().child(&line).build();
            self.jump_list.append(&row);
            rows.push(Some(i));
        }
        self.jump_rows.replace(rows);
    }

    /// Zum Monat springen. Fehlt er noch, laden erst die Monate davor.
    fn jump_to_bucket(self: &Rc<Self>, i: usize) {
        let start = self.bucket_starts.borrow().get(i).copied();
        match start {
            Some(pos) => self.scroll_to_top(pos),
            None => {
                self.jump_target.set(Some(i));
                if let Some(b) = self.buckets.borrow().get(i) {
                    self.title
                        .set_subtitle(&format!("Lade bis {} …", month_title(b)));
                }
                self.load_next_bucket();
            }
        }
    }

    /// Bild an den oberen Rand holen. `scroll_to` bewegt nur so weit, bis es
    /// sichtbar ist (beim Abwärtsspringen also unten), danach wird nachgeschoben.
    fn scroll_to_top(self: &Rc<Self>, pos: u32) {
        self.grid.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(50),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    let mut child = this.grid.first_child();
                    while let Some(c) = child {
                        if let Some(tile) = c.first_child().and_downcast::<Tile>()
                            && tile.position() == pos
                            && let Some(p) =
                                c.compute_point(&this.grid, &gtk::graphene::Point::zero())
                        {
                            let adj = this.scroller.vadjustment();
                            adj.set_value(adj.value() + p.y() as f64);
                            break;
                        }
                        child = c.next_sibling();
                    }
                    this.show_month();
                }
            ),
        );
    }

    /// Zurück aus der Einzelansicht: Fokus und Auswahl auf das zuletzt gezeigte Bild.
    pub fn focus_item(&self, pos: u32) {
        if pos < self.store.n_items() {
            self.grid.scroll_to(
                pos,
                gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                None,
            );
        }
    }

    pub fn connect_activate(&self, f: impl Fn(u32) + 'static) {
        self.on_activate.replace(Some(Box::new(f)));
    }

    pub fn connect_selection(&self, f: impl Fn() + 'static) {
        self.on_selection.replace(Some(Box::new(move |()| f())));
    }

    pub fn origin(&self) -> Option<Origin> {
        self.origin.borrow().clone()
    }

    pub fn selected_photos(&self) -> Vec<Photo> {
        let set = self.selection.selection();
        (0..set.size())
            .filter_map(|i| self.store.item(set.nth(i as u32)).and_downcast::<Photo>())
            .collect()
    }

    pub fn is_busy(&self) -> bool {
        self.cancel.borrow().is_some()
    }

    fn selection_changed(&self) {
        let n = self.selection.selection().size() as usize;
        if !self.is_busy() {
            self.count.set_label(&if n == 0 {
                String::new()
            } else {
                format!("{} ausgewählt", bilder(n))
            });
            self.bar.set_revealed(n > 0);
        }
        if let Some(f) = self.on_selection.borrow().as_ref() {
            f(());
        }
    }

    fn next_generation(&self) -> u64 {
        let g = self.generation.get() + 1;
        self.generation.set(g);
        self.pending_buckets.borrow_mut().clear();
        self.loading_more.set(false);
        self.buckets.borrow_mut().clear();
        self.bucket_starts.borrow_mut().clear();
        self.jump_target.set(None);
        self.jump.set_visible(false);
        self.month_reveal.set_reveal_child(false);
        for p in self.store.iter::<Photo>().flatten() {
            p.cancel_thumbnail();
        }
        self.store.remove_all();
        self.unwatch();
        g
    }

    /// Ordner beobachten: neue, gelöschte und geänderte Dateien erscheinen ohne Neuladen.
    /// Höchstens so viele Ordner beobachten; inotify hat ein Limit pro Nutzer,
    /// und das teilen sich alle Programme.
    const MAX_WATCHED: usize = 1000;

    fn unwatch(&self) {
        for m in self.monitors.take() {
            m.cancel();
        }
    }

    /// Ordner beobachten, mit Unterordnern auch den ganzen Baum: neue,
    /// gelöschte und geänderte Dateien erscheinen ohne Neuladen.
    fn watch(self: &Rc<Self>, dir: &Path) {
        self.unwatch();
        let generation = self.generation.get();
        let (dir, recursive) = (dir.to_owned(), self.subfolders.is_active());
        let task = gio::spawn_blocking(move || {
            if recursive {
                blick_core::library::subdirs(&dir, Self::MAX_WATCHED)
            } else {
                vec![dir]
            }
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(dirs) = task.await else { return };
                if this.generation.get() != generation {
                    return;
                }
                let monitors: Vec<gio::FileMonitor> =
                    dirs.iter().filter_map(|d| this.monitor_one(d)).collect();
                this.monitors.replace(monitors);
            }
        ));
    }

    fn monitor_one(self: &Rc<Self>, dir: &Path) -> Option<gio::FileMonitor> {
        let monitor = gio::File::for_path(dir)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            .ok()?;
        monitor.connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, file, _, event| {
                let Some(path) = file.path() else { return };
                // Neuer Unterordner: Baum neu beobachten, sobald abgeglichen wird.
                let new_dir = event == gio::FileMonitorEvent::Created && path.is_dir();
                if !new_dir && !blick_core::library::is_supported(&path) {
                    return;
                }
                if new_dir {
                    this.rewatch.set(true);
                }
                if event == gio::FileMonitorEvent::ChangesDoneHint {
                    this.changed.borrow_mut().push(path);
                }
                this.schedule_refresh();
            }
        ));
        Some(monitor)
    }

    fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_pending.replace(true) {
            return;
        }
        // Kopiervorgänge erzeugen Dutzende Ereignisse; gesammelt abgleichen.
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(400),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.refresh_pending.set(false);
                    this.refresh();
                }
            ),
        );
    }

    /// Ordner neu lesen und mit dem Bogen abgleichen. Vorhandene Einträge bleiben
    /// erhalten (samt Thumbnail und Auswahl), nur Neues und Gelöschtes ändert sich.
    pub fn refresh(self: &Rc<Self>) {
        let Some(Origin::Folder(dir)) = self.origin() else {
            return;
        };
        let generation = self.generation.get();
        let scan = self.scan(dir);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(Ok(paths)) = scan.await else { return };
                if this.generation.get() != generation {
                    return;
                }
                let changed: Vec<PathBuf> = this.changed.take();
                let existing: std::collections::HashMap<PathBuf, Photo> = this
                    .store
                    .iter::<Photo>()
                    .flatten()
                    .filter_map(|p| p.path().map(|path| (path.to_owned(), p.clone())))
                    .collect();
                let same =
                    paths.len() == existing.len() && paths.iter().all(|p| existing.contains_key(p));
                for path in &changed {
                    if let Some(photo) = existing.get(path) {
                        this.ctx.previews.remove_where(|k| &k.0 == path);
                        photo.reload(&this.ctx);
                    }
                }
                if same {
                    return;
                }
                let photos: Vec<Photo> = paths
                    .into_iter()
                    .enumerate()
                    .map(|(i, path)| match existing.get(&path) {
                        Some(p) => {
                            p.set_number(i as u32 + 1);
                            p.clone()
                        }
                        None => Photo::local(path, i as u32 + 1),
                    })
                    .collect();
                this.title.set_subtitle(&this.count_text(photos.len()));
                this.store.splice(0, this.store.n_items(), &photos);
                this.stack
                    .set_visible_child_name(if photos.is_empty() { "empty" } else { "grid" });
            }
        ));
    }

    /// Einträge sofort entfernen (nach dem Löschen), ohne auf den Monitor zu warten.
    pub fn remove_paths(&self, paths: &[PathBuf]) {
        for i in (0..self.store.n_items()).rev() {
            if let Some(p) = self.store.item(i).and_downcast::<Photo>()
                && p.path().is_some_and(|p| paths.iter().any(|x| x == p))
            {
                p.cancel_thumbnail();
                self.store.remove(i);
            }
        }
        for (i, p) in self.store.iter::<Photo>().flatten().enumerate() {
            p.set_number(i as u32 + 1);
        }
        self.title
            .set_subtitle(&bilder(self.store.n_items() as usize));
    }

    fn show_empty(&self, title: &str, description: &str) {
        self.empty.set_title(title);
        self.empty.set_description(Some(description));
        self.stack.set_visible_child_name("empty");
    }

    /// Liest den Ordner, mit oder ohne Unterordner. Ein laufender Scan des
    /// vorigen Ordners wird abgebrochen.
    fn scan(&self, dir: PathBuf) -> gio::JoinHandle<blick_core::Result<Vec<PathBuf>>> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_cancel
            .replace(cancel.clone())
            .store(true, Ordering::Relaxed);
        if self.subfolders.is_active() {
            gio::spawn_blocking(move || blick_core::library::scan_recursive(&dir, &cancel))
        } else {
            gio::spawn_blocking(move || blick_core::library::scan_dir(&dir))
        }
    }

    fn count_text(&self, n: usize) -> String {
        if self.subfolders.is_active() {
            format!("{}, mit Unterordnern", bilder(n))
        } else {
            bilder(n)
        }
    }

    pub fn open_folder(self: &Rc<Self>, dir: PathBuf) {
        let generation = self.next_generation();
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        self.page.set_title(&name);
        self.title.set_title(&name);
        self.title.set_subtitle("");
        self.origin.replace(Some(Origin::Folder(dir.clone())));
        self.subfolders.set_visible(true);
        self.stack.set_visible_child_name("loading");
        self.selection_changed();

        self.watch(&dir);
        // Ordner auf dem NAS können dauern, also nicht im UI-Thread lesen.
        let scan = self.scan(dir);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = scan.await;
                if this.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(Ok(paths)) if paths.is_empty() => this.show_empty(
                        "Keine Bilder in diesem Ordner",
                        if this.subfolders.is_active() {
                            "Auch in den Unterordnern nicht. Blick zeigt JPEG, PNG, WebP, TIFF, HEIC, AVIF, JPEG XL und Kamera-RAW. Mit Strg+O öffnest du einen anderen Ordner."
                        } else {
                            "Unterordner sind ausgeblendet; der Ordner-Knopf oben rechts blendet sie ein. Mit Strg+O öffnest du einen anderen Ordner."
                        },
                    ),
                    Ok(Ok(paths)) => {
                        let photos: Vec<Photo> =
                            paths.into_iter().enumerate().map(|(i, p)| Photo::local(p, i as u32 + 1)).collect();
                        this.title.set_subtitle(&this.count_text(photos.len()));
                        this.store.splice(0, 0, &photos);
                        this.stack.set_visible_child_name("grid");
                    }
                    Ok(Err(e)) => this.show_empty("Ordner lässt sich nicht lesen", &e.to_string()),
                    Err(_) => this.show_empty("Ordner lässt sich nicht lesen", "Der Lesevorgang ist abgestürzt."),
                }
            }
        ));
    }

    /// Lädt die Monatsliste und dann Monat für Monat, solange gescrollt wird.
    pub fn open_immich(self: &Rc<Self>, id: Option<String>, name: String) {
        let generation = self.next_generation();
        self.subfolders.set_visible(false);
        self.page.set_title(&name);
        self.title.set_title(&name);
        self.title.set_subtitle("Immich");
        self.origin.replace(Some(Origin::Immich {
            id: id.clone(),
            name,
        }));
        self.selection_changed();
        let Some(client) = self.ctx.immich() else {
            self.show_empty(
                "Immich ist nicht verbunden",
                "Trag Server und API-Schlüssel in den Einstellungen ein.",
            );
            return;
        };
        self.stack.set_visible_child_name("loading");
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result =
                    runtime::spawn(async move { Ok(client.time_buckets(id.as_deref()).await?) })
                        .await;
                if this.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(buckets) if buckets.is_empty() => this.show_empty(
                        "Dieses Album ist leer",
                        "Wähle im Kontaktbogen Bilder aus und lade sie mit Strg+U hoch.",
                    ),
                    Ok(buckets) => {
                        let total: u64 = buckets.iter().map(|b| b.count).sum();
                        this.title
                            .set_subtitle(&format!("Immich, {}", bilder(total as usize)));
                        this.buckets.replace(buckets.clone());
                        this.fill_jump_list();
                        this.jump.set_visible(true);
                        this.pending_buckets.replace(buckets.into());
                        this.stack.set_visible_child_name("grid");
                        // Zwei Monate vorab, damit der Bogen gleich gefüllt ist.
                        this.load_next_bucket();
                    }
                    Err(e) => this.show_empty("Immich antwortet nicht", &format!("{e:#}")),
                }
            }
        ));
    }

    fn load_next_bucket(self: &Rc<Self>) {
        if self.loading_more.get() {
            return;
        }
        let Some(Origin::Immich { id, .. }) = self.origin() else {
            return;
        };
        let Some(bucket) = self.pending_buckets.borrow_mut().pop_front() else {
            return;
        };
        let Some(client) = self.ctx.immich() else {
            return;
        };
        self.loading_more.set(true);
        let generation = self.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = runtime::spawn(async move {
                    Ok(client
                        .bucket_assets(id.as_deref(), &bucket.time_bucket)
                        .await?)
                })
                .await;
                if this.generation.get() != generation {
                    return;
                }
                this.loading_more.set(false);
                match result {
                    Ok(assets) => {
                        let start = this.store.n_items();
                        this.bucket_starts.borrow_mut().push(start);
                        let photos: Vec<Photo> = assets
                            .into_iter()
                            .enumerate()
                            .map(|(i, a)| Photo::remote(a, start + i as u32 + 1))
                            .collect();
                        this.store.splice(start, 0, &photos);
                        if let Some(target) = this.jump_target.get() {
                            let loaded = this.bucket_starts.borrow().len();
                            if target < loaded {
                                this.jump_target.set(None);
                                this.restore_subtitle();
                                let pos = this.bucket_starts.borrow()[target];
                                this.scroll_to_top(pos);
                            } else {
                                this.load_next_bucket();
                            }
                            return;
                        }
                        // Reicht der Inhalt noch nicht über den Bildschirm, gleich weiter.
                        let adj = this.scroller.vadjustment();
                        if this.store.n_items() < 60 || adj.upper() <= adj.page_size() * 3.0 {
                            this.load_next_bucket();
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Immich-Monat nicht geladen: {e:#}");
                        if this.jump_target.take().is_some() {
                            this.restore_subtitle();
                        }
                    }
                }
            }
        ));
    }

    fn restore_subtitle(&self) {
        let total: u64 = self.buckets.borrow().iter().map(|b| b.count).sum();
        self.title
            .set_subtitle(&format!("Immich, {}", bilder(total as usize)));
    }

    /// Fortschritt im unteren Balken; der Schalter bricht den Lauf ab.
    pub fn set_busy(&self, cancel: Arc<AtomicBool>) {
        self.cancel.replace(Some(cancel));
        self.actions.set_visible(false);
        self.busy.set_visible(true);
        self.count.set_label("");
        self.progress.set_fraction(0.0);
        self.progress.set_text(None);
        self.bar.set_revealed(true);
    }

    pub fn set_progress(&self, verb: &str, done: usize, total: usize) {
        self.progress
            .set_fraction(done as f64 / total.max(1) as f64);
        self.progress
            .set_text(Some(&format!("{verb} {done} von {total}")));
    }

    pub fn set_idle(&self) {
        self.cancel.replace(None);
        self.busy.set_visible(false);
        self.actions.set_visible(true);
        self.selection_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_title_is_german() {
        let b = |t: &str| TimeBucket {
            time_bucket: t.into(),
            count: 3,
        };
        assert_eq!(month_title(&b("2026-10-01T00:00:00.000Z")), "Oktober 2026");
        assert_eq!(month_title(&b("2019-03-01T00:00:00.000Z")), "März 2019");
        assert_eq!(month_title(&b("???")), "???");
    }
}
