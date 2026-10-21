//! Duplicate search: choose locations, search, mark hits and clean up.
//! The search itself lives in `blick-dupes`; this is only the UI.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use blick_dupes::hashdb::HashDb;
use blick_dupes::immich::ImmichStorage;
use blick_dupes::results::{self, Action, Keep, Marks};
use blick_dupes::gio_storage::GioStorage;
use blick_dupes::{
    Entry, Group, LocalStorage, Member, Mode, Options, Progress, Scanner, SourceSpec, Stage,
    Storage,
};
use gtk::{gdk, gio, glib};

use crate::ctx::Ctx;
use crate::runtime;

/// Location in the list: path (starts with `/`) or address (`smb://…`).
#[derive(Clone, PartialEq)]
struct Location {
    uri: String,
    reference: bool,
}

impl Location {
    fn parse(s: &str) -> Option<Self> {
        let (flag, uri) = s.split_once('|')?;
        (!uri.is_empty()).then(|| Self { uri: uri.to_owned(), reference: flag == "R" })
    }

    fn encode(&self) -> String {
        format!("{}|{}", if self.reference { "R" } else { "N" }, self.uri)
    }

    fn local(&self) -> bool {
        self.uri.starts_with('/')
    }
}

/// Local like `LocalStorage`, but with trash through GIO.
struct TrashingLocal(LocalStorage);

impl Storage for TrashingLocal {
    fn label(&self) -> String {
        self.0.label()
    }
    fn list(&self, c: &AtomicBool, f: &mut dyn FnMut(Entry)) -> blick_dupes::Result<()> {
        self.0.list(c, f)
    }
    fn read_range(&self, e: &Entry, o: u64, l: usize) -> blick_dupes::Result<Vec<u8>> {
        self.0.read_range(e, o, l)
    }
    fn read_all(&self, e: &Entry) -> blick_dupes::Result<Vec<u8>> {
        self.0.read_all(e)
    }
    fn local_path(&self, e: &Entry) -> Option<PathBuf> {
        self.0.local_path(e)
    }
    fn trash(&self, e: &Entry) -> blick_dupes::Result<()> {
        gio::File::for_path(&e.uri)
            .trash(gio::Cancellable::NONE)
            .map_err(|x| blick_dupes::DupeError::Other(format!("{}: {}", e.uri, x.message())))
    }
    fn delete(&self, e: &Entry) -> blick_dupes::Result<()> {
        std::fs::remove_file(&e.uri).map_err(|x| blick_dupes::DupeError::io(&e.uri, x))
    }
}

#[derive(Clone)]
enum RowData {
    Header { group: usize },
    Item { group: usize, index: usize },
}

#[derive(Default)]
struct State {
    groups: Vec<Group>,
    marks: Marks,
    storages: Vec<Arc<dyn Storage>>,
}

pub struct DupesView {
    pub page: adw::NavigationPage,
    ctx: Rc<Ctx>,
    settings: gio::Settings,
    toasts: adw::ToastOverlay,
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    start: gtk::Button,
    locations: RefCell<Vec<Location>>,
    loc_group: adw::PreferencesGroup,
    loc_rows: RefCell<Vec<adw::ActionRow>>,
    immich_row: adw::SwitchRow,
    mode: adw::ComboRow,
    threshold: adw::SpinRow,
    rotation: adw::SwitchRow,
    aspect: adw::SwitchRow,
    fuzzy: adw::SwitchRow,
    min_kib: adw::SpinRow,
    progress_label: gtk::Label,
    progress_bar: gtk::ProgressBar,
    cancel: RefCell<Arc<AtomicBool>>,
    store: gio::ListStore,
    state: Rc<RefCell<State>>,
    summary: gtk::Label,
    actions: gtk::Box,
    busy: Cell<bool>,
}

const MODES: [&str; 4] = ["Pictures (similar)", "Content (identical)", "File names", "Folders"];

pub fn human_size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn file_name(uri: &str) -> String {
    let n = uri.rsplit('/').next().unwrap_or(uri);
    glib::Uri::unescape_string(n, None).map_or_else(|| n.to_owned(), |s| s.to_string())
}

fn parent_of(uri: &str) -> String {
    let p = uri.rsplit_once('/').map_or("", |x| x.0);
    let p = p.strip_prefix("file://").unwrap_or(p);
    glib::Uri::unescape_string(p, None).map_or_else(|| p.to_owned(), |s| s.to_string())
}

impl DupesView {
    pub fn new(
        ctx: Rc<Ctx>,
        settings: gio::Settings,
        toasts: adw::ToastOverlay,
        window: adw::ApplicationWindow,
    ) -> Rc<Self> {
        let loc_group = adw::PreferencesGroup::builder()
            .title("Where should it search?")
            .description("Mark locations as \"reference\": files there are always kept.")
            .build();
        let immich_row = adw::SwitchRow::builder()
            .title("Immich library")
            .subtitle("Compares by the server's checksums, no download")
            .build();
        let mode = adw::ComboRow::builder()
            .title("Search type")
            .model(&gtk::StringList::new(&MODES))
            .build();
        let threshold = adw::SpinRow::with_range(50.0, 100.0, 1.0);
        threshold.set_title("Minimum similarity (%)");
        let rotation = adw::SwitchRow::builder()
            .title("Find rotated pictures")
            .build();
        let aspect = adw::SwitchRow::builder()
            .title("Allow different aspect ratios")
            .subtitle("Also finds cropped versions, but more false hits")
            .build();
        let fuzzy = adw::SwitchRow::builder()
            .title("Similar words count as equal")
            .subtitle("One typo apart, file names only")
            .build();
        let min_kib = adw::SpinRow::with_range(0.0, 1_000_000.0, 10.0);
        min_kib.set_title("Minimum size (KB)");

        let find = adw::PreferencesGroup::builder().title("Search").build();
        find.add(&mode);
        find.add(&threshold);
        find.add(&rotation);
        find.add(&aspect);
        find.add(&fuzzy);
        find.add(&min_kib);

        let add_folder = gtk::Button::builder()
            .label("Folder …")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let add_server = gtk::Button::builder()
            .label("Server …")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        head.append(&add_folder);
        head.append(&add_server);
        loc_group.set_header_suffix(Some(&head));

        let setup_page = adw::PreferencesPage::new();
        setup_page.add(&loc_group);
        let imm_group = adw::PreferencesGroup::new();
        imm_group.add(&immich_row);
        setup_page.add(&imm_group);
        setup_page.add(&find);

        let progress_label = gtk::Label::builder().label("Starting …").build();
        let progress_bar = gtk::ProgressBar::new();
        let cancel_btn = gtk::Button::builder()
            .label("Cancel")
            .halign(gtk::Align::Center)
            .build();
        let run_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .width_request(360)
            .build();
        run_box.append(&adw::Spinner::new());
        run_box.append(&progress_label);
        run_box.append(&progress_bar);
        run_box.append(&cancel_btn);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let state = Rc::new(RefCell::new(State::default()));
        let summary = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        summary.add_css_class("dim-label");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(store.clone()))),
            None::<gtk::SignalListItemFactory>,
        );
        list.add_css_class("navigation-sidebar");
        list.set_single_click_activate(false);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.set_margin_start(12);
        bar.set_margin_end(12);
        bar.append(&summary);
        bar.append(&actions);
        let results_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        results_box.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        results_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        results_box.append(&bar);

        let empty = adw::StatusPage::builder()
            .icon_name("edit-copy-symbolic")
            .title("No duplicates found")
            .description("Search again with different settings or a lower similarity.")
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&setup_page, Some("setup"));
        stack.add_named(&run_box, Some("run"));
        stack.add_named(&results_box, Some("results"));
        stack.add_named(&empty, Some("empty"));

        let start = gtk::Button::builder()
            .label("Search")
            .css_classes(["suggested-action"])
            .build();
        let back = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("New search")
            .visible(false)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&start);
        header.pack_end(&back);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Duplicates")
            .tag("dupes")
            .child(&view)
            .build();

        let this = Rc::new(Self {
            page,
            ctx,
            settings,
            toasts,
            window,
            stack,
            start,
            locations: RefCell::new(Vec::new()),
            loc_group,
            loc_rows: RefCell::new(Vec::new()),
            immich_row,
            mode,
            threshold,
            rotation,
            aspect,
            fuzzy,
            min_kib,
            progress_label,
            progress_bar,
            cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
            store,
            state,
            summary,
            actions,
            busy: Cell::new(false),
        });
        this.build_list(&list);
        this.build_actions();
        this.load_settings();

        let w = Rc::downgrade(&this);
        this.start.connect_clicked({
            let w = w.clone();
            move |_| {
                if let Some(t) = w.upgrade() {
                    t.run();
                }
            }
        });
        back.connect_clicked({
            let w = w.clone();
            move |b| {
                if let Some(t) = w.upgrade() {
                    t.show_setup();
                    b.set_visible(false);
                }
            }
        });
        // After the search the back button becomes visible (see `finish`).
        this.stack.connect_visible_child_name_notify({
            let back = back.clone();
            let start = this.start.clone();
            move |s| {
                let name = s.visible_child_name().unwrap_or_default();
                back.set_visible(name == "results" || name == "empty");
                start.set_visible(name == "setup");
            }
        });
        cancel_btn.connect_clicked({
            let w = w.clone();
            move |_| {
                if let Some(t) = w.upgrade() {
                    t.cancel.borrow().store(true, Ordering::Relaxed);
                }
            }
        });
        add_folder.connect_clicked({
            let w = w.clone();
            move |_| {
                if let Some(t) = w.upgrade() {
                    t.choose_folder();
                }
            }
        });
        add_server.connect_clicked({
            let w = w.clone();
            move |_| {
                if let Some(t) = w.upgrade() {
                    t.ask_server();
                }
            }
        });
        let sync = {
            let w = w.clone();
            move || {
                if let Some(t) = w.upgrade() {
                    t.sync_option_rows();
                }
            }
        };
        this.mode.connect_selected_notify({
            let sync = sync.clone();
            move |_| sync()
        });
        this.sync_option_rows();
        this
    }

    /// When the page opens: offer the library folders, Immich only when connected.
    pub fn prepare(self: &Rc<Self>, library_roots: &[PathBuf]) {
        {
            let mut locs = self.locations.borrow_mut();
            for r in library_roots {
                let uri = r.to_string_lossy().into_owned();
                if !locs.iter().any(|l| l.uri == uri) {
                    locs.push(Location { uri, reference: false });
                }
            }
        }
        self.immich_row.set_visible(self.ctx.immich().is_some());
        self.render_locations();
        if self.stack.visible_child_name().as_deref() != Some("results") {
            self.show_setup();
        }
    }

    fn show_setup(&self) {
        self.stack.set_visible_child_name("setup");
    }

    fn sync_option_rows(&self) {
        let m = self.mode.selected();
        self.threshold.set_visible(m == 0 || m == 2);
        self.rotation.set_visible(m == 0);
        self.aspect.set_visible(m == 0);
        self.fuzzy.set_visible(m == 2);
        self.min_kib.set_visible(m != 3);
    }

    fn load_settings(&self) {
        let locs: Vec<Location> = self
            .settings
            .strv("dupes-locations")
            .iter()
            .filter_map(|s| Location::parse(s))
            .collect();
        *self.locations.borrow_mut() = locs;
        self.mode.set_selected(self.settings.int("dupes-mode").clamp(0, 3) as u32);
        self.threshold.set_value(self.settings.int("dupes-threshold") as f64);
        self.immich_row.set_active(self.settings.boolean("dupes-immich"));
        self.aspect.set_active(self.settings.boolean("dupes-any-aspect"));
        self.rotation.set_active(self.settings.boolean("dupes-rotation"));
    }

    fn save_settings(&self) {
        let enc: Vec<String> = self.locations.borrow().iter().map(Location::encode).collect();
        let refs: Vec<&str> = enc.iter().map(String::as_str).collect();
        let _ = self.settings.set_strv("dupes-locations", refs.as_slice());
        let _ = self.settings.set_int("dupes-mode", self.mode.selected() as i32);
        let _ = self.settings.set_int("dupes-threshold", self.threshold.value() as i32);
        let _ = self.settings.set_boolean("dupes-immich", self.immich_row.is_active());
        let _ = self.settings.set_boolean("dupes-any-aspect", self.aspect.is_active());
        let _ = self.settings.set_boolean("dupes-rotation", self.rotation.is_active());
    }

    fn render_locations(self: &Rc<Self>) {
        for r in self.loc_rows.borrow_mut().drain(..) {
            self.loc_group.remove(&r);
        }
        let mut rows = Vec::new();
        for (i, loc) in self.locations.borrow().iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&parent_of(&format!("{}/x", loc.uri))))
                .subtitle(if loc.local() { "This computer" } else { "Remote" })
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(if loc.local() {
                "folder-symbolic"
            } else {
                "network-server-symbolic"
            }));
            let refsw = gtk::Switch::builder()
                .valign(gtk::Align::Center)
                .active(loc.reference)
                .tooltip_text("Reference: never suggest for deletion")
                .build();
            let reflabel = gtk::Label::new(Some("Reference"));
            reflabel.add_css_class("dim-label");
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text("Remove from list")
                .build();
            row.add_suffix(&reflabel);
            row.add_suffix(&refsw);
            row.add_suffix(&del);
            let w = Rc::downgrade(self);
            refsw.connect_active_notify({
                let w = w.clone();
                move |s| {
                    if let Some(t) = w.upgrade() {
                        if let Some(l) = t.locations.borrow_mut().get_mut(i) {
                            l.reference = s.is_active();
                        }
                    }
                }
            });
            del.connect_clicked(move |_| {
                if let Some(t) = w.upgrade() {
                    t.locations.borrow_mut().remove(i);
                    t.render_locations();
                }
            });
            self.loc_group.add(&row);
            rows.push(row);
        }
        *self.loc_rows.borrow_mut() = rows;
    }

    fn add_location(self: &Rc<Self>, uri: String) {
        {
            let mut l = self.locations.borrow_mut();
            if l.iter().any(|x| x.uri == uri) {
                return;
            }
            l.push(Location { uri, reference: false });
        }
        self.render_locations();
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Add folder").modal(true).build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Ok(Some(path)) = dialog
                    .select_folder_future(Some(&this.window))
                    .await
                    .map(|f| f.path())
                {
                    this.add_location(path.to_string_lossy().into_owned());
                }
            }
        ));
    }

    fn ask_server(self: &Rc<Self>) {
        let entry = gtk::Entry::builder()
            .placeholder_text("smb://nas.example/photos")
            .activates_default(true)
            .build();
        entry.set_input_purpose(gtk::InputPurpose::Url);
        let dialog = adw::AlertDialog::new(
            Some("Add server"),
            Some("Address of a shared folder, such as smb://nas/photos, sftp://host/path or davs://host/dav. GNOME asks for credentials when connecting."),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("add", "Connect")]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if dialog.choose_future(Some(&this.window)).await != "add" {
                    return;
                }
                let uri = entry.text().trim().trim_end_matches('/').to_owned();
                if !uri.contains("://") {
                    this.toast("That isn't a server address (it's missing something like \"smb://\").");
                    return;
                }
                let file = gio::File::for_uri(&uri);
                let op = gtk::MountOperation::new(Some(&this.window));
                match file
                    .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&op))
                    .await
                {
                    Ok(()) => {}
                    Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => {}
                    Err(e) => {
                        this.toast(&format!("Connecting failed: {}", e.message()));
                        return;
                    }
                }
                match file
                    .query_info_future("standard::type", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
                    .await
                {
                    Ok(_) => this.add_location(uri),
                    Err(e) => this.toast(&format!("Folder not readable: {}", e.message())),
                }
            }
        ));
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn options(&self) -> Options {
        let mode = match self.mode.selected() {
            0 => Mode::Pictures,
            1 => Mode::Contents,
            2 => Mode::Names,
            _ => Mode::Folders,
        };
        Options {
            mode,
            min_size: (self.min_kib.value() as u64 * 1000).max(1),
            threshold: self.threshold.value() as u8,
            ignore_rotation: self.rotation.is_active(),
            aspect_tolerance: (!self.aspect.is_active()).then_some(0.02),
            fuzzy_words: self.fuzzy.is_active(),
            ..Options::default()
        }
    }

    fn run(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let mut specs = Vec::new();
        let mut storages: Vec<Arc<dyn Storage>> = Vec::new();
        for loc in self.locations.borrow().iter() {
            let st: Arc<dyn Storage> = if loc.local() {
                Arc::new(TrashingLocal(LocalStorage::new(&loc.uri)))
            } else {
                Arc::new(GioStorage::new(&loc.uri))
            };
            storages.push(st.clone());
            specs.push(SourceSpec { storage: st, reference: loc.reference });
        }
        if self.immich_row.is_active() {
            if let Some(client) = self.ctx.immich() {
                let st: Arc<dyn Storage> = Arc::new(ImmichStorage {
                    client,
                    handle: runtime::runtime().handle().clone(),
                    album: None,
                    label: "Immich".into(),
                });
                storages.push(st.clone());
                specs.push(SourceSpec { storage: st, reference: false });
            }
        }
        if specs.is_empty() {
            self.toast("Please add at least one location.");
            return;
        }
        self.save_settings();
        let db_path = glib::user_cache_dir().join("blick").join("dupes.db");
        let db = match HashDb::open(&db_path) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                tracing::warn!("Cache: {e}");
                Arc::new(HashDb::in_memory().expect("in-memory database"))
            }
        };
        let scanner = Scanner { sources: specs, options: self.options(), db };
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = cancel.clone();
        self.busy.set(true);
        self.progress_bar.set_fraction(0.0);
        self.progress_label.set_text("Reading locations …");
        self.stack.set_visible_child_name("run");

        let (tx, rx) = async_channel::unbounded::<Result<Vec<Group>, String>>();
        let (ptx, prx) = async_channel::unbounded::<Progress>();
        std::thread::Builder::new()
            .name("blick-dupes".into())
            .spawn(move || {
                let r = scanner.run(&cancel, &|p| {
                    let _ = ptx.try_send(p);
                });
                let _ = tx.send_blocking(r.map_err(|e| e.to_string()));
            })
            .expect("duplicate search thread failed to start");
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(p) = prx.recv().await {
                    this.show_progress(p);
                }
            }
        ));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let r = rx.recv().await.unwrap_or_else(|_| Err("cancelled".into()));
                this.busy.set(false);
                match r {
                    Ok(groups) => this.finish(groups, storages),
                    Err(e) => {
                        if e != "cancelled" {
                            this.toast(&format!("Search failed: {e}"));
                        }
                        this.show_setup();
                    }
                }
            }
        ));
    }

    fn show_progress(&self, p: Progress) {
        let text = match p.stage {
            Stage::Listing => format!("Reading files … {}", p.done),
            Stage::Probing => "Pre-check …".to_owned(),
            Stage::Hashing => "Comparing files …".to_owned(),
            Stage::Grouping => "Grouping hits …".to_owned(),
        };
        self.progress_label.set_text(&text);
        if p.total > 0 && p.stage != Stage::Listing {
            self.progress_bar.set_fraction((p.done as f64 / p.total as f64).min(1.0));
        } else {
            self.progress_bar.pulse();
        }
    }

    fn finish(self: &Rc<Self>, groups: Vec<Group>, storages: Vec<Arc<dyn Storage>>) {
        {
            let mut s = self.state.borrow_mut();
            s.marks = Marks::none(&groups);
            s.groups = groups;
            s.storages = storages;
        }
        self.rebuild();
        let empty = self.state.borrow().groups.is_empty();
        self.stack.set_visible_child_name(if empty { "empty" } else { "results" });
    }

    /// Flat list: a header row per group, the members below.
    fn rebuild(&self) {
        self.store.remove_all();
        let s = self.state.borrow();
        let mut rows = Vec::new();
        for (g, grp) in s.groups.iter().enumerate() {
            rows.push(glib::BoxedAnyObject::new(RowData::Header { group: g }));
            for i in 0..grp.members.len() {
                rows.push(glib::BoxedAnyObject::new(RowData::Item { group: g, index: i }));
            }
        }
        self.store.splice(0, 0, &rows);
        drop(s);
        self.update_summary();
    }

    /// Marks have changed: rebind all visible rows.
    fn refresh_rows(&self) {
        let n = self.store.n_items();
        self.store.items_changed(0, n, n);
        self.update_summary();
    }

    fn update_summary(&self) {
        let s = self.state.borrow();
        let files: usize = s.groups.iter().map(|g| g.members.len()).sum();
        let text = format!(
            "{} groups, {} files · {} marked ({})",
            s.groups.len(),
            files,
            s.marks.count(),
            human_size(s.marks.bytes(&s.groups))
        );
        self.summary.set_text(&text);
        let any = s.marks.count() > 0;
        let mut child = self.actions.first_child();
        while let Some(w) = child {
            if w.widget_name() == "needs-marks" {
                w.set_sensitive(any);
            }
            child = w.next_sibling();
        }
    }

    fn build_actions(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        let mark_menu = gio::Menu::new();
        let keep = gio::Menu::new();
        for (id, label) in [
            ("resolution", "All but the highest resolution"),
            ("biggest", "All but the largest file"),
            ("newest", "All but the newest"),
            ("oldest", "All but the oldest"),
            ("shortest", "All but the shortest path"),
        ] {
            keep.append(Some(label), Some(&format!("dupes.mark-{id}")));
        }
        mark_menu.append_section(None, &keep);
        let other = gio::Menu::new();
        other.append(Some("Mark by path part …"), Some("dupes.mark-text"));
        other.append(Some("Clear marks"), Some("dupes.mark-none"));
        mark_menu.append_section(None, &other);
        let mark_btn = gtk::MenuButton::builder()
            .label("Mark")
            .menu_model(&mark_menu)
            .build();

        let act_menu = gio::Menu::new();
        let a1 = gio::Menu::new();
        a1.append(Some("Move to trash"), Some("dupes.trash"));
        a1.append(Some("Move to …"), Some("dupes.move"));
        a1.append(Some("Replace with hardlink"), Some("dupes.hardlink"));
        a1.append(Some("Delete permanently"), Some("dupes.delete"));
        act_menu.append_section(None, &a1);
        let a2 = gio::Menu::new();
        a2.append(Some("Export as CSV …"), Some("dupes.csv"));
        act_menu.append_section(None, &a2);
        let act_btn = gtk::MenuButton::builder()
            .label("Action")
            .menu_model(&act_menu)
            .css_classes(["suggested-action"])
            .build();
        act_btn.set_widget_name("needs-marks");
        self.actions.append(&mark_btn);
        self.actions.append(&act_btn);

        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: Rc<dyn Fn(&Rc<Self>)>| {
            let a = gio::SimpleAction::new(name, None);
            let w = w.clone();
            a.connect_activate(move |_, _| {
                if let Some(t) = w.upgrade() {
                    f(&t);
                }
            });
            group.add_action(&a);
        };
        for (id, k) in [
            ("resolution", Keep::Resolution),
            ("biggest", Keep::BiggestFile),
            ("newest", Keep::Newest),
            ("oldest", Keep::Oldest),
            ("shortest", Keep::ShortestPath),
        ] {
            add(
                &format!("mark-{id}"),
                Rc::new(move |t| {
                    let m = Marks::all_but_best(&t.state.borrow().groups, k);
                    t.state.borrow_mut().marks = m;
                    t.refresh_rows();
                }),
            );
        }
        add(
            "mark-none",
            Rc::new(|t| {
                let m = Marks::none(&t.state.borrow().groups);
                t.state.borrow_mut().marks = m;
                t.refresh_rows();
            }),
        );
        add("mark-text", Rc::new(|t| t.ask_mark_text()));
        add("trash", Rc::new(|t| t.confirm_apply(Action::Trash, "Trash", false)));
        add("delete", Rc::new(|t| t.confirm_apply(Action::Delete, "Permanently delete", true)));
        add("hardlink", Rc::new(|t| t.confirm_apply(Action::Hardlink(Keep::ShortestPath), "Hardlink", false)));
        add("move", Rc::new(|t| t.choose_move_target()));
        add("csv", Rc::new(|t| t.export_csv()));
        self.page.insert_action_group("dupes", Some(&group));
    }

    fn ask_mark_text(self: &Rc<Self>) {
        let entry = gtk::Entry::builder()
            .placeholder_text("/Downloads/ or copy")
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::new(
            Some("Mark by path part"),
            Some("Marks all files whose path contains the text. At least one file stays unmarked in every group."),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("ok", "Mark")]);
        dialog.set_default_response(Some("ok"));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if dialog.choose_future(Some(&this.window)).await != "ok" {
                    return;
                }
                let needle = entry.text().to_string();
                if needle.is_empty() {
                    return;
                }
                let m = Marks::by_substring(&this.state.borrow().groups, &needle);
                this.state.borrow_mut().marks = m;
                this.refresh_rows();
            }
        ));
    }

    fn confirm_apply(self: &Rc<Self>, action: Action, verb: &'static str, destructive: bool) {
        let (count, bytes, full) = {
            let s = self.state.borrow();
            (s.marks.count(), s.marks.bytes(&s.groups), s.marks.fully_marked_groups())
        };
        if count == 0 {
            return;
        }
        if !full.is_empty() {
            self.toast(&format!(
                "Everything is marked in {} group(s). At least one file has to stay.",
                full.len()
            ));
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&format!("{verb} {count} files?")),
            Some(&format!("Together {}.", human_size(bytes))),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("ok", "Run")]);
        dialog.set_response_appearance(
            "ok",
            if destructive { adw::ResponseAppearance::Destructive } else { adw::ResponseAppearance::Suggested },
        );
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if dialog.choose_future(Some(&this.window)).await == "ok" {
                    this.apply(action).await;
                }
            }
        ));
    }

    fn choose_move_target(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Target folder").modal(true).build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Ok(Some(path)) = dialog.select_folder_future(Some(&this.window)).await.map(|f| f.path()) {
                    this.apply(Action::MoveTo(path)).await;
                }
            }
        ));
    }

    async fn apply(self: &Rc<Self>, action: Action) {
        let (groups, marks, storages) = {
            let s = self.state.borrow();
            (s.groups.clone(), s.marks.clone(), s.storages.clone())
        };
        let g2 = groups.clone();
        let m2 = marks.clone();
        let res = gio::spawn_blocking(move || results::apply(&g2, &m2, &action, &storages)).await;
        let outcome = match res {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                self.toast(&e.to_string());
                return;
            }
            Err(_) => {
                self.toast("The action crashed.");
                return;
            }
        };
        // Take finished members out of the results; groups with nothing left drop out.
        let failed: std::collections::HashSet<&str> =
            outcome.failed.iter().map(|(u, _)| u.as_str()).collect();
        let mut kept = Vec::new();
        for (g, grp) in groups.into_iter().enumerate() {
            let members: Vec<Member> = grp
                .members
                .into_iter()
                .enumerate()
                .filter(|(i, m)| !marks.0[g][*i] || failed.contains(m.entry.uri.as_str()))
                .map(|(_, m)| m)
                .collect();
            if members.len() > 1 && members.iter().any(|m| !m.reference) {
                kept.push(Group { members, score: grp.score });
            }
        }
        {
            let mut s = self.state.borrow_mut();
            s.marks = Marks::none(&kept);
            s.groups = kept;
        }
        self.rebuild();
        if self.state.borrow().groups.is_empty() {
            self.stack.set_visible_child_name("empty");
        }
        let mut text = format!("{} files done ({})", outcome.done, human_size(outcome.bytes));
        if let Some((u, e)) = outcome.failed.first() {
            text.push_str(&format!(". {} failed, e.g. {}: {e}", outcome.failed.len(), file_name(u)));
        }
        self.toasts.add_toast(adw::Toast::builder().title(text).timeout(10).build());
    }

    fn export_csv(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Export results")
            .initial_name("duplicates.csv")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(Some(path)) = dialog.save_future(Some(&this.window)).await.map(|f| f.path()) else {
                    return;
                };
                let csv = {
                    let s = this.state.borrow();
                    results::export_csv(&s.groups, &s.marks)
                };
                match std::fs::write(&path, csv) {
                    Ok(()) => this.toast("CSV saved."),
                    Err(e) => this.toast(&format!("Saving failed: {e}")),
                }
            }
        ));
    }

    fn build_list(self: &Rc<Self>, list: &gtk::ListView) {
        let factory = gtk::SignalListItemFactory::new();
        let w = Rc::downgrade(self);
        factory.connect_setup({
            let w = w.clone();
            move |_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
                let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                root.set_margin_top(4);
                root.set_margin_bottom(4);
                root.set_margin_start(12);
                root.set_margin_end(12);
                let check = gtk::CheckButton::new();
                check.set_valign(gtk::Align::Center);
                let pic = gtk::Picture::builder()
                    .content_fit(gtk::ContentFit::Cover)
                    .width_request(56)
                    .height_request(56)
                    .build();
                pic.set_size_request(56, 56);
                pic.add_css_class("card");
                let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
                text.set_hexpand(true);
                text.set_valign(gtk::Align::Center);
                let title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
                let sub = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Start).build();
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
                let meta = gtk::Label::builder().xalign(0.0).build();
                meta.add_css_class("caption");
                text.append(&title);
                text.append(&sub);
                text.append(&meta);
                let badge = gtk::Label::new(Some("Reference"));
                badge.add_css_class("caption");
                badge.add_css_class("accent");
                badge.set_valign(gtk::Align::Center);
                root.append(&check);
                root.append(&pic);
                root.append(&text);
                root.append(&badge);
                item.set_child(Some(&root));
                let w = w.clone();
                let weak_item = item.downgrade();
                check.connect_toggled(move |c| {
                    let (Some(t), Some(item)) = (w.upgrade(), weak_item.upgrade()) else { return };
                    let Some(o) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
                    let RowData::Item { group, index } = o.borrow::<RowData>().clone() else { return };
                    let differs = t.state.borrow().marks.0[group][index] != c.is_active();
                    if differs {
                        let groups = t.state.borrow().groups.clone();
                        t.state.borrow_mut().marks.toggle(group, index, &groups);
                        t.update_summary();
                    }
                });
            }
        });
        factory.connect_bind({
            let w = w.clone();
            move |_, obj| {
                let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(t) = w.upgrade() else { return };
                let Some(o) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
                let data = o.borrow::<RowData>().clone();
                let root = item.child().and_downcast::<gtk::Box>().unwrap();
                let check = root.first_child().and_downcast::<gtk::CheckButton>().unwrap();
                let pic = check.next_sibling().and_downcast::<gtk::Picture>().unwrap();
                let text = pic.next_sibling().and_downcast::<gtk::Box>().unwrap();
                let badge = text.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let title = text.first_child().and_downcast::<gtk::Label>().unwrap();
                let sub = title.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let meta = sub.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let s = t.state.borrow();
                match data {
                    RowData::Header { group } => {
                        let g = &s.groups[group];
                        let waste: u64 = g.members.iter().skip(1).map(|m| m.entry.size).sum();
                        check.set_visible(false);
                        pic.set_visible(false);
                        badge.set_visible(false);
                        sub.set_visible(false);
                        meta.set_visible(false);
                        title.set_markup(&format!(
                            "<b>Group {}</b> · {} % · {} files · {} surplus",
                            group + 1,
                            g.score,
                            g.members.len(),
                            human_size(waste)
                        ));
                        item.set_activatable(false);
                        item.set_selectable(false);
                    }
                    RowData::Item { group, index } => {
                        let m = s.groups[group].members[index].clone();
                        let marked = s.marks.0[group][index];
                        drop(s);
                        check.set_visible(true);
                        pic.set_visible(true);
                        sub.set_visible(true);
                        meta.set_visible(true);
                        check.set_active(marked);
                        check.set_sensitive(!m.reference);
                        badge.set_visible(m.reference);
                        title.set_markup(&glib::markup_escape_text(&file_name(&m.entry.uri)));
                        sub.set_text(&parent_of(&m.entry.uri));
                        let mut parts = vec![human_size(m.entry.size)];
                        if let Some((w, h)) = m.entry.dims {
                            parts.push(format!("{w} × {h}"));
                        }
                        if m.entry.mtime > 0 {
                            if let Ok(d) = glib::DateTime::from_unix_local(m.entry.mtime) {
                                if let Ok(f) = d.format("%d.%m.%Y") {
                                    parts.push(f.to_string());
                                }
                            }
                        }
                        meta.set_text(&parts.join(" · "));
                        item.set_activatable(true);
                        pic.set_paintable(None::<&gdk::Paintable>);
                        let ctx = t.ctx.clone();
                        let item = item.clone();
                        let me = o.clone();
                        glib::spawn_future_local(async move {
                            if let Some(tex) = fetch_texture(&ctx, &m, false).await {
                                if item.item().as_ref() == Some(me.upcast_ref::<glib::Object>()) {
                                    pic.set_paintable(Some(&tex));
                                }
                            }
                        });
                    }
                }
            }
        });
        list.set_factory(Some(&factory));
        list.connect_activate({
            let w = w.clone();
            move |lv, pos| {
                let Some(t) = w.upgrade() else { return };
                let Some(o) = lv.model().and_then(|m| m.item(pos)).and_downcast::<glib::BoxedAnyObject>() else { return };
                if let RowData::Item { group, index } = o.borrow::<RowData>().clone() {
                    t.compare(group, index);
                }
            }
        });
    }

    /// All files of the group side by side: the clicked one first, then the best member.
    fn compare(self: &Rc<Self>, group: usize, index: usize) {
        let members: Vec<Member> = {
            let s = self.state.borrow();
            let g = &s.groups[group];
            let best = results::best(g, Keep::Resolution);
            let mut order = vec![index];
            if best != index {
                order.push(best);
            }
            let rest: Vec<usize> = (0..g.members.len()).filter(|i| !order.contains(i)).collect();
            order.extend(rest);
            order.into_iter().map(|i| g.members[i].clone()).collect()
        };
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_homogeneous(true);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_bottom(12);
        for m in &members {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
            // From three pictures on every column gets a minimum width, the rest scrolls.
            col.set_width_request(if members.len() > 2 { 340 } else { 1 });
            let pic = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .vexpand(true)
                .hexpand(true)
                .build();
            let info = gtk::Label::builder()
                .label(format!(
                    "{}\n{}{}",
                    file_name(&m.entry.uri),
                    human_size(m.entry.size),
                    m.entry.dims.map_or(String::new(), |(w, h)| format!(" · {w} × {h}"))
                ))
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build();
            info.add_css_class("caption");
            col.append(&pic);
            col.append(&info);
            content.append(&col);
            let ctx = self.ctx.clone();
            let m = m.clone();
            glib::spawn_future_local(async move {
                if let Some(tex) = fetch_texture(&ctx, &m, true).await {
                    pic.set_paintable(Some(&tex));
                }
            });
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scroller));
        let dialog = adw::Dialog::builder()
            .title("Comparison")
            .content_width(1000)
            .content_height(620)
            .child(&view)
            .build();
        dialog.present(Some(&self.window));
    }
}

/// Image for a tile (`big = false`) or the comparison. Local through the thumbnail loader
/// or directly, Immich through the server, other locations only up to 40 MB.
async fn fetch_texture(ctx: &Rc<Ctx>, m: &Member, big: bool) -> Option<gdk::Texture> {
    let uri = m.entry.uri.clone();
    let px_max = if big { 1400 } else { 160 };
    if let Some(rest) = uri.strip_prefix("immich://") {
        let id = rest.split('/').next()?.to_owned();
        let client = ctx.immich()?;
        let size = if big { blick_immich::ThumbSize::Preview } else { blick_immich::ThumbSize::Thumbnail };
        let px = runtime::spawn(async move {
            let b = client.thumbnail(&id, size).await?;
            Ok(tokio::task::spawn_blocking(move || blick_core::decode::load_bytes(&b)).await??)
        })
        .await
        .ok()?;
        return Some(crate::ctx::texture(px));
    }
    let local: Option<PathBuf> = if uri.starts_with('/') {
        Some(PathBuf::from(&uri))
    } else if uri.starts_with("file://") {
        gio::File::for_uri(&uri).path()
    } else {
        None
    };
    if let Some(path) = local {
        if !blick_dupes::sig::is_picture(&uri) {
            return None;
        }
        if !big {
            let (rx, _cancel) = ctx.thumbs.request(path);
            return rx.recv().await.ok()?.ok().map(crate::ctx::texture);
        }
        let px = gio::spawn_blocking(move || blick_core::decode::load_preview(Path::new(&path), px_max, px_max))
            .await
            .ok()?
            .ok()?;
        return Some(crate::ctx::texture(px));
    }
    if m.entry.size > 40_000_000 || !blick_dupes::sig::is_picture(&uri) {
        return None;
    }
    let px = gio::spawn_blocking(move || {
        let bytes = GioStorage::new("").read_all(&Entry {
            uri,
            size: 0,
            mtime: 0,
            id: None,
            checksum: None,
            dims: None,
        });
        bytes
            .ok()
            .and_then(|b| blick_core::decode::load_bytes(&b).ok())
            .and_then(|p| p.fit_within(px_max, px_max).ok())
    })
    .await
    .ok()??;
    Some(crate::ctx::texture(px))
}
