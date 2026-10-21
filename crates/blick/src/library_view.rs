//! Seitenleisten-Abschnitt „Bibliothek“: Ordner wie `~/Photos`, rekursiv
//! durchsucht. Jeder Unterordner mit Bildern erscheint als Album.
//!
//! Der Scan läuft in einem eigenen Thread und meldet Alben, sobald sie gefunden
//! sind. Bei großen Bäumen auf dem NAS füllt sich die Liste also nach und nach,
//! statt minutenlang leer zu bleiben.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use blick_core::library::Album;
use gtk::prelude::*;
use gtk::{gio, glib};

/// Wie oft die Liste während eines Scans höchstens neu aufgebaut wird.
const RENDER_DELAY_MS: u64 = 200;

struct Root {
    path: PathBuf,
    albums: Vec<Album>,
    scanning: bool,
    expanded: bool,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
}

enum Entry {
    Root(PathBuf),
    Album(PathBuf),
    Group,
    Add,
}

enum Msg {
    Album(Album),
    Done(Result<(), String>),
}

type Callback = Box<dyn Fn(PathBuf)>;

pub struct LibraryView {
    pub list: gtk::ListBox,
    settings: gio::Settings,
    roots: RefCell<Vec<Root>>,
    entries: RefCell<Vec<Entry>>,
    render_pending: Cell<bool>,
    selected: RefCell<Option<PathBuf>>,
    on_open: RefCell<Option<Callback>>,
    on_add: RefCell<Option<Box<dyn Fn()>>>,
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn row(
    depth: usize,
    icon: Option<&str>,
    label: &str,
    suffix: Option<&str>,
) -> (gtk::ListBoxRow, gtk::Box) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.set_margin_start(depth as i32 * 14);
    if let Some(icon) = icon {
        b.append(&gtk::Image::from_icon_name(icon));
    }
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
    let name = match suffix {
        Some(s) => format!("{label}, {s} Bilder"),
        None => label.to_owned(),
    };
    row.update_property(&[gtk::accessible::Property::Label(&name)]);
    (row, b)
}

impl LibraryView {
    pub fn new(settings: gio::Settings) -> Rc<Self> {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        let this = Rc::new(Self {
            list,
            settings,
            roots: RefCell::new(Vec::new()),
            entries: RefCell::new(Vec::new()),
            render_pending: Cell::new(false),
            selected: RefCell::new(None),
            on_open: RefCell::new(None),
            on_add: RefCell::new(None),
        });
        this.list.connect_row_activated(glib::clone!(
            #[weak]
            this,
            move |_, row| this.activated(row.index() as usize)
        ));
        let saved: Vec<PathBuf> = this
            .settings
            .strv("library-roots")
            .iter()
            .map(|s| PathBuf::from(s.as_str()))
            .collect();
        for path in saved {
            this.push_root(path);
        }
        this.render();
        this
    }

    pub fn connect_open(&self, f: impl Fn(PathBuf) + 'static) {
        self.on_open.replace(Some(Box::new(f)));
    }

    pub fn connect_add(&self, f: impl Fn() + 'static) {
        self.on_add.replace(Some(Box::new(f)));
    }

    pub fn unselect(&self) {
        self.selected.replace(None);
        self.list.unselect_all();
    }

    /// Nimmt einen Ordner in die Bibliothek auf und startet den Scan.
    /// Steckt er schon in einer vorhandenen Bibliothek, passiert nichts.
    pub fn add_root(self: &Rc<Self>, path: PathBuf) {
        if self
            .roots
            .borrow()
            .iter()
            .any(|r| path.starts_with(&r.path))
        {
            return;
        }
        // Ein neuer Oberordner ersetzt Bibliotheken, die er schon enthält.
        let inner: Vec<PathBuf> = self
            .roots
            .borrow()
            .iter()
            .filter(|r| r.path.starts_with(&path))
            .map(|r| r.path.clone())
            .collect();
        for p in inner {
            self.remove_root(&p);
        }
        self.push_root(path);
        self.save();
        self.render();
    }

    fn push_root(self: &Rc<Self>, path: PathBuf) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.roots.borrow_mut().push(Root {
            path: path.clone(),
            albums: Vec::new(),
            scanning: true,
            expanded: true,
            error: None,
            cancel: cancel.clone(),
        });
        self.scan(path, cancel);
    }

    pub fn remove_root(&self, path: &Path) {
        self.roots.borrow_mut().retain(|r| {
            if r.path == path {
                r.cancel.store(true, Ordering::Relaxed);
            }
            r.path != path
        });
        self.save();
        self.render();
    }

    /// Alle Bibliotheken neu durchsuchen, etwa nach einem Import.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.borrow().iter().map(|r| r.path.clone()).collect()
    }

    pub fn rescan(self: &Rc<Self>) {
        let paths: Vec<PathBuf> = self.roots.borrow().iter().map(|r| r.path.clone()).collect();
        for r in self.roots.borrow().iter() {
            r.cancel.store(true, Ordering::Relaxed);
        }
        self.roots.borrow_mut().clear();
        for p in paths {
            self.push_root(p);
        }
        self.render();
    }

    fn save(&self) {
        let paths: Vec<String> = self
            .roots
            .borrow()
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let _ = self.settings.set_strv("library-roots", refs.as_slice());
    }

    fn scan(self: &Rc<Self>, root: PathBuf, cancel: Arc<AtomicBool>) {
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let scan_root = root.clone();
        std::thread::Builder::new()
            .name("blick-scan".into())
            .spawn(move || {
                let r = blick_core::library::scan_tree(&scan_root, &cancel, |a| {
                    let _ = tx.send_blocking(Msg::Album(a));
                });
                let _ = tx.send_blocking(Msg::Done(r.map_err(|e| e.to_string())));
            })
            .expect("Scan-Thread ließ sich nicht starten");

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(msg) = rx.recv().await {
                    let mut roots = this.roots.borrow_mut();
                    // Inzwischen entfernt: Rest verwerfen.
                    let Some(r) = roots.iter_mut().find(|r| r.path == root) else {
                        return;
                    };
                    match msg {
                        Msg::Album(a) => r.albums.push(a),
                        Msg::Done(res) => {
                            r.scanning = false;
                            r.error = res.err().filter(|e| !e.contains("abgebrochen"));
                        }
                    }
                    drop(roots);
                    this.schedule_render();
                }
            }
        ));
    }

    fn schedule_render(self: &Rc<Self>) {
        if self.render_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(RENDER_DELAY_MS),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.render_pending.set(false);
                    this.render();
                }
            ),
        );
    }

    fn render(&self) {
        while let Some(r) = self.list.row_at_index(0) {
            self.list.remove(&r);
        }
        let mut entries = Vec::new();
        let selected = self.selected.borrow().clone();
        let mut select_row = None;

        for root in self.roots.borrow().iter() {
            let total: usize = root.albums.iter().map(|a| a.count).sum();
            let suffix = if root.scanning {
                format!("{total} …")
            } else {
                total.to_string()
            };
            let icon = if root.expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            };
            let (r, b) = row(0, Some(icon), &display_name(&root.path), Some(&suffix));
            r.set_tooltip_text(Some(&match &root.error {
                Some(e) => format!("{}\n{e}", root.path.display()),
                None => root.path.display().to_string(),
            }));
            let remove = gtk::Button::builder()
                .icon_name("list-remove-symbolic")
                .tooltip_text("Aus der Bibliothek entfernen")
                .valign(gtk::Align::Center)
                .build();
            remove.add_css_class("flat");
            // Der Knopf geht über eine Fensteraktion, damit die Zeile das View-Objekt nicht halten muss.
            remove.set_action_name(Some("win.library-remove"));
            remove.set_action_target_value(Some(&root.path.to_string_lossy().to_variant()));
            b.append(&remove);
            self.list.append(&r);
            entries.push(Entry::Root(root.path.clone()));

            if !root.expanded {
                continue;
            }
            // Zwischenordner ohne eigene Bilder trotzdem zeigen, sonst hängen
            // `2024/Urlaub` und `2024/Garten` ohne `2024` in der Luft.
            let album_rels: BTreeSet<&str> = root.albums.iter().map(|a| a.rel.as_str()).collect();
            let mut shown: BTreeSet<String> = BTreeSet::new();
            let mut albums: Vec<&Album> =
                root.albums.iter().filter(|a| !a.rel.is_empty()).collect();
            albums.sort_by_key(|a| a.rel.to_lowercase());
            if let Some(top) = root.albums.iter().find(|a| a.rel.is_empty()) {
                albums.insert(0, top);
            }
            for a in albums {
                let parts: Vec<&str> = if a.rel.is_empty() {
                    vec![]
                } else {
                    a.rel.split('/').collect()
                };
                for depth in 1..parts.len() {
                    let prefix = parts[..depth].join("/");
                    if !album_rels.contains(prefix.as_str()) && shown.insert(prefix) {
                        let (g, _) = row(depth, None, parts[depth - 1], None);
                        g.set_activatable(false);
                        g.set_selectable(false);
                        g.add_css_class("dim-label");
                        self.list.append(&g);
                        entries.push(Entry::Group);
                    }
                }
                shown.insert(a.rel.clone());
                let name = parts.last().copied().unwrap_or("Bilder direkt hier");
                let (r, _) = row(parts.len().max(1), None, name, Some(&a.count.to_string()));
                r.set_tooltip_text(Some(&a.dir.display().to_string()));
                if selected.as_deref() == Some(a.dir.as_path()) {
                    select_row = Some(r.clone());
                }
                self.list.append(&r);
                entries.push(Entry::Album(a.dir.clone()));
            }
        }
        let (add, _) = row(
            0,
            Some("list-add-symbolic"),
            "Ordner zur Bibliothek hinzufügen …",
            None,
        );
        add.set_tooltip_text(Some("Durchsucht den Ordner mit allen Unterordnern"));
        self.list.append(&add);
        entries.push(Entry::Add);
        self.entries.replace(entries);
        if let Some(r) = select_row {
            self.list.select_row(Some(&r));
        }
    }

    fn activated(&self, index: usize) {
        let entry = match self.entries.borrow().get(index) {
            Some(Entry::Root(p)) => Some(Entry::Root(p.clone())),
            Some(Entry::Album(p)) => Some(Entry::Album(p.clone())),
            Some(Entry::Add) => Some(Entry::Add),
            _ => None,
        };
        match entry {
            Some(Entry::Root(p)) => {
                if let Some(r) = self.roots.borrow_mut().iter_mut().find(|r| r.path == p) {
                    r.expanded = !r.expanded;
                }
                self.render();
            }
            Some(Entry::Album(dir)) => {
                self.selected.replace(Some(dir.clone()));
                if let Some(f) = self.on_open.borrow().as_ref() {
                    f(dir);
                }
            }
            Some(Entry::Add) => {
                self.list.unselect_all();
                if let Some(f) = self.on_add.borrow().as_ref() {
                    f();
                }
            }
            _ => {}
        }
    }
}
