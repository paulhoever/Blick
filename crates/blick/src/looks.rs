//! Look library: bundled looks plus imported files under
//! `$XDG_DATA_HOME/blick/looks/`, and the picker dialog with previews.
//!
//! Files are only parsed when they're needed: the RawTherapee collection
//! has almost 300 HaldCLUTs of 1728×1728 pixels each, and loading all of them
//! at startup would cost seconds.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use blick_core::Pixels;
use blick_core::look::Look;
use gtk::glib::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use crate::ctx::texture;

/// RawTherapee's Film Simulation Collection, CC BY-SA 4.0.
pub const RAWTHERAPEE_URL: &str = "https://rawtherapee.com/shared/HaldCLUT.zip";
const RAWTHERAPEE_DIR: &str = "RawTherapee";
const RAWTHERAPEE_NOTE: &str = "RawTherapee Film Simulation Collection, CC BY-SA 4.0\n\
Pat David, Pavlov Dmitry, Michael Ezra. https://rawpedia.rawtherapee.com/Film_Simulation\n\
Brand names in the file names only say which film the look imitates.\n";

pub fn looks_dir() -> PathBuf {
    glib::user_data_dir().join("blick").join("looks")
}

// ---------- Entry ----------

mod imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::LookItem)]
    pub struct LookItem {
        #[property(get)]
        pub name: RefCell<String>,
        #[property(get)]
        pub group: RefCell<String>,
        #[property(get, set, nullable)]
        pub preview: RefCell<Option<gdk::Texture>>,
        pub path: RefCell<Option<PathBuf>>,
        pub look: OnceCell<Arc<Look>>,
        pub preview_for: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LookItem {
        const NAME: &'static str = "BlickLookItem";
        type Type = super::LookItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for LookItem {}
}

glib::wrapper! {
    pub struct LookItem(ObjectSubclass<imp::LookItem>);
}

impl LookItem {
    fn builtin(look: Look) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().name.replace(look.name.clone());
        obj.imp().group.replace("Blick".into());
        let _ = obj.imp().look.set(Arc::new(look));
        obj
    }

    fn file(path: PathBuf, group: String) -> Self {
        let obj: Self = glib::Object::new();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().replace(['_'], " "))
            .unwrap_or_default();
        obj.imp().name.replace(name);
        obj.imp().group.replace(group);
        obj.imp().path.replace(Some(path));
        obj
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.imp().path.borrow().clone()
    }

    /// Loads the file on first access. Call from worker threads through `load_blocking`.
    pub fn cached(&self) -> Option<Arc<Look>> {
        self.imp().look.get().cloned()
    }

    fn remember(&self, look: Arc<Look>) {
        let _ = self.imp().look.set(look);
    }
}

/// Loads a look off the UI thread.
pub async fn load(item: &LookItem) -> anyhow::Result<Arc<Look>> {
    if let Some(l) = item.cached() {
        return Ok(l);
    }
    let path = item
        .path()
        .ok_or_else(|| anyhow::anyhow!("look without a file"))?;
    let look = gio::spawn_blocking(move || Look::load(&path).map(Arc::new))
        .await
        .map_err(|_| anyhow::anyhow!("loading crashed"))??;
    item.remember(look.clone());
    Ok(look)
}

/// For thumbnails from worker threads: load the look for a sidecar key.
/// Keeps loaded looks, a HaldCLUT isn't re-read for every picture.
pub fn resolve_key(key: &str) -> Option<Arc<Look>> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static CACHE: LazyLock<Mutex<HashMap<String, Arc<Look>>>> = LazyLock::new(Default::default);
    if let Some(l) = CACHE.lock().unwrap().get(key) {
        return Some(l.clone());
    }
    let path = looks_dir().join(key);
    // Only inside the look folder, a sidecar mustn't load arbitrary files.
    if !path.starts_with(looks_dir()) || key.contains("..") {
        return None;
    }
    let look = Arc::new(Look::load(&path).ok()?);
    CACHE.lock().unwrap().insert(key.to_owned(), look.clone());
    Some(look)
}

// ---------- Library ----------

pub struct Library {
    pub store: gio::ListStore,
}

impl Library {
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self {
            store: gio::ListStore::new::<LookItem>(),
        });
        this.reload();
        this
    }

    /// Bundled ones first, then all files in the look folder by group and name.
    pub fn reload(&self) {
        let mut items: Vec<LookItem> = Look::builtins()
            .into_iter()
            .map(LookItem::builtin)
            .collect();
        let root = looks_dir();
        let mut files = Vec::new();
        collect(&root, &mut files);
        files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        for p in files {
            let group = p
                .parent()
                .and_then(|d| d.strip_prefix(&root).ok())
                .map(|r| {
                    r.iter()
                        .map(|c| c.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            items.push(LookItem::file(
                p,
                if group.is_empty() {
                    "Imported".into()
                } else {
                    group
                },
            ));
        }
        self.store.splice(0, self.store.n_items(), &items);
    }

    /// Key for the sidecar: path relative to the look folder, for
    /// bundled looks `blick:<Name>`.
    pub fn key_of(look: &LookItem) -> String {
        match look.path() {
            Some(p) => p
                .strip_prefix(looks_dir())
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned(),
            None => format!("blick:{}", look.name()),
        }
    }

    pub fn find(&self, key: &str) -> Option<LookItem> {
        self.store
            .iter::<LookItem>()
            .flatten()
            .find(|l| Self::key_of(l) == key)
    }

    /// Entry for a loaded look (via the pointer).
    pub fn item_for(&self, look: &Arc<Look>) -> Option<LookItem> {
        self.store
            .iter::<LookItem>()
            .flatten()
            .find(|l| l.cached().is_some_and(|c| Arc::ptr_eq(&c, look)))
    }

    pub fn has_rawtherapee(&self) -> bool {
        looks_dir().join(RAWTHERAPEE_DIR).is_dir()
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            collect(&p, out);
        } else if Look::is_look_file(&p) {
            out.push(p);
        }
    }
}

/// Result of an import, for the message afterwards.
#[derive(Default)]
pub struct ImportReport {
    pub imported: usize,
    pub failed: Vec<(PathBuf, String)>,
    /// Lightroom fields Blick doesn't map, per preset.
    pub ignored: Vec<(String, Vec<String>)>,
}

/// Copies look files into the look folder. Folders keep their structure
/// (the folder name becomes the group). Every file is parsed first,
/// broken ones don't end up in the library.
pub fn import(sources: &[PathBuf]) -> ImportReport {
    let root = looks_dir();
    let mut report = ImportReport::default();
    for src in sources {
        let mut files = Vec::new();
        let base = if src.is_dir() {
            collect(src, &mut files);
            src.parent().map(Path::to_owned).unwrap_or_default()
        } else {
            files.push(src.clone());
            src.parent().map(Path::to_owned).unwrap_or_default()
        };
        for f in files {
            match Look::load(&f) {
                Ok(look) => {
                    let rel = f.strip_prefix(&base).unwrap_or(&f);
                    let dst = root.join(rel);
                    let copied = dst
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::copy(&f, &dst));
                    match copied {
                        Ok(_) => {
                            report.imported += 1;
                            if !look.ignored.is_empty() {
                                report
                                    .ignored
                                    .push((look.name.clone(), look.ignored.clone()));
                            }
                        }
                        Err(e) => report.failed.push((f, e.to_string())),
                    }
                }
                Err(e) => report.failed.push((f, e.to_string())),
            }
        }
    }
    report
}

/// Downloads the RawTherapee collection (about 420 MB) and unpacks only the PNGs.
/// `progress(loaded, total)` is called from the network task.
pub async fn download_rawtherapee(
    progress: impl Fn(u64, u64) + Send + 'static,
) -> anyhow::Result<usize> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let root = looks_dir();
    tokio::fs::create_dir_all(&root).await?;
    let tmp = root.join(".rawtherapee-download.zip");
    let _ = rustls::crypto::ring::default_provider().install_default();
    let resp = reqwest::get(RAWTHERAPEE_URL).await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let mut done = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    file.flush().await?;
    drop(file);

    let count = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let target = root.join(RAWTHERAPEE_DIR);
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&tmp)?)?;
        let mut n = 0;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            if name
                .extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("png"))
            {
                continue;
            }
            // "HaldCLUT/Color/Kodak/….png" → "RawTherapee/Color/Kodak/….png"
            let rel: PathBuf = name.components().skip(1).collect();
            let dst = target.join(rel);
            if let Some(dir) = dst.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::io::copy(&mut entry, &mut std::fs::File::create(&dst)?)?;
            n += 1;
        }
        std::fs::write(target.join("LICENSE.txt"), RAWTHERAPEE_NOTE)?;
        // The downloaded ZIP is our own temporary file.
        std::fs::remove_file(&tmp)?;
        Ok(n)
    })
    .await??;
    Ok(count)
}

// ---------- Picker dialog ----------

const PREVIEW_PX: u32 = 112;

/// Shows all looks with a preview of the current picture. `base` is the picture
/// at screen size; it is shrunk once to tile size.
pub fn pick(
    parent: &impl IsA<gtk::Widget>,
    library: Rc<Library>,
    base: Option<Arc<Pixels>>,
    on_pick: impl Fn(Option<LookItem>) + 'static,
) {
    let small: Option<Arc<Pixels>> = base.and_then(|b| {
        (*b).clone()
            .fit_within(PREVIEW_PX * 2, PREVIEW_PX * 2)
            .ok()
            .map(Arc::new)
    });
    let generation = glib::random_int() as u64 + 1;

    let filter = gtk::CustomFilter::new(|_| true);
    let filtered = gtk::FilterListModel::new(Some(library.store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::builder()
        .model(&filtered)
        .autoselect(false)
        .can_unselect(true)
        .build();
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let pic = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .build();
        pic.set_size_request(PREVIEW_PX as i32, PREVIEW_PX as i32);
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .build();
        name.add_css_class("caption");
        let group = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .width_chars(1)
            .build();
        group.set_css_classes(&["caption", "dim-label"]);
        let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
        b.set_margin_start(6);
        b.set_margin_end(6);
        b.set_margin_top(6);
        b.set_margin_bottom(6);
        let frame = gtk::Overlay::builder()
            .child(&pic)
            .css_classes(["card"])
            .overflow(gtk::Overflow::Hidden)
            .margin_bottom(4)
            .build();
        b.append(&frame);
        b.append(&name);
        b.append(&group);
        item.set_child(Some(&b));
    });
    let small_for_bind = small.clone();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let (Some(look), Some(b)) = (
            item.item().and_downcast::<LookItem>(),
            item.child().and_downcast::<gtk::Box>(),
        ) else {
            return;
        };
        let frame = b
            .first_child()
            .and_downcast::<gtk::Overlay>()
            .expect("Overlay");
        let pic = frame
            .child()
            .and_downcast::<gtk::Picture>()
            .expect("Picture");
        let name = frame
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("Label");
        let group = name
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("Label");
        name.set_label(&look.name());
        // Only the last part of the group, "Kodak" instead of "RawTherapee / Color / Kodak".
        let g = look.group();
        group.set_label(g.rsplit(" / ").next().unwrap_or(&g));
        group.set_tooltip_text(Some(&g));
        b.set_tooltip_text(Some(&look.name()));
        pic.set_paintable(look.preview().as_ref());
        let binding = look.bind_property("preview", &pic, "paintable").build();
        unsafe { b.set_data("binding", binding) };
        // Preview only for visible tiles and only once per dialog.
        if let Some(src) = small_for_bind.clone()
            && look.imp().preview_for.replace(generation) != generation
        {
            glib::spawn_future_local(async move {
                let Ok(l) = load(&look).await else { return };
                let out = gio::spawn_blocking(move || {
                    let mut px = (*src).clone();
                    l.render(1.0).apply(&mut px, 1);
                    px
                })
                .await;
                if let Ok(px) = out {
                    look.set_preview(Some(texture(px)));
                }
            });
        }
    });
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("ListItem");
        if let Some(b) = item.child().and_downcast::<gtk::Box>()
            && let Some(binding) = unsafe { b.steal_data::<glib::Binding>("binding") }
        {
            binding.unbind();
        }
    });

    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.set_max_columns(8);
    grid.set_single_click_activate(true);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search looks, such as Portra or Velvia")
        .hexpand(true)
        .build();
    search.connect_search_changed(glib::clone!(
        #[weak]
        filter,
        move |s| {
            let q = s.text().to_lowercase();
            filter.set_filter_func(move |o| {
                let it = o.downcast_ref::<LookItem>().expect("LookItem");
                q.is_empty()
                    || it.name().to_lowercase().contains(&q)
                    || it.group().to_lowercase().contains(&q)
            });
        }
    ));

    let none = gtk::Button::with_label("No look");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&search));
    header.pack_start(&none);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scroller));

    if !library.has_rawtherapee() {
        // Permanent notice as an AdwBanner, as the HIG suggest.
        let banner = adw::Banner::builder()
            .title("RawTherapee film simulations: free, CC BY-SA 4.0, about 420 MB")
            .button_label("Download")
            .action_name("win.looks-download")
            .revealed(true)
            .build();
        view.add_top_bar(&banner);
    }

    let dialog = adw::Dialog::builder()
        .title("Choose look")
        .content_width(900)
        .content_height(640)
        .child(&view)
        .build();
    let on_pick = Rc::new(on_pick);
    grid.connect_activate(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        on_pick,
        move |g, pos| {
            let item = g
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<LookItem>();
            on_pick(item);
            dialog.close();
        }
    ));
    none.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        on_pick,
        move |_| {
            on_pick(None);
            dialog.close();
        }
    ));
    dialog.present(Some(parent));
    search.grab_focus();
}
