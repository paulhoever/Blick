//! Videos: schlichter Player für die Einzelansicht und Standbilder für den Bogen.
//!
//! Abgespielt wird über GStreamer (`playbin` + `gtk4paintablesink` aus der
//! GNOME-Laufzeit, mit Hardware-Dekodierung, wo VA-API sie hergibt). Blick transkodiert nichts
//! selbst: Lokale Dateien laufen direkt, bei Immich kommt die Fassung, die der
//! Server schon für den Browser erzeugt hat. Jedes Video startet stumm.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;

use adw::prelude::*;
use blick_core::{CoreError, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::{gdk, gio, glib};
use image::{DynamicImage, RgbaImage};

const VOLUME_DEFAULT: f64 = 0.6;
const VOLUME_STEP: f64 = 0.1;

// ---------------------------------------------------------------- Standbild

fn err(path: &Path, e: impl std::fmt::Display) -> CoreError {
    CoreError::Decode {
        path: path.to_owned(),
        reason: e.to_string(),
    }
}

/// Wartet auf Preroll (oder Fehler) nach einem Zustandswechsel oder Sprung.
fn wait_async_done(bus: &gst::Bus, path: &Path) -> Result<()> {
    let msg = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::AsyncDone, gst::MessageType::Error],
        )
        .ok_or_else(|| err(path, "Zeitüberschreitung beim Öffnen"))?;
    match msg.view() {
        gst::MessageView::Error(e) => Err(err(path, e.error())),
        _ => Ok(()),
    }
}

/// Ein Bild aus dem ersten Zehntel des Videos (höchstens bei 1 s), aufrecht
/// gedreht (Handyvideos tragen die Drehung nur als Tag) und verkleinert.
/// Läuft in Worker-Threads.
pub fn grab_frame(path: &Path, max_edge: u32) -> Result<DynamicImage> {
    gst_init();
    let uri = gio::File::for_path(path).uri();
    let sink_bin = gst::parse::bin_from_description(
        "videoflip video-direction=auto ! videoconvertscale \
         ! video/x-raw,format=RGBA,pixel-aspect-ratio=1/1 \
         ! appsink name=sink sync=false max-buffers=1 drop=true",
        true,
    )
    .map_err(|e| err(path, e))?;
    let appsink = sink_bin
        .by_name("sink")
        .and_downcast::<gst_app::AppSink>()
        .ok_or_else(|| err(path, "appsink fehlt"))?;
    let fakesink = gst::ElementFactory::make("fakesink")
        .build()
        .map_err(|e| err(path, e))?;
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", uri.as_str())
        .property("video-sink", &sink_bin)
        .property("audio-sink", &fakesink)
        .build()
        .map_err(|e| err(path, e))?;
    // Nur Bild dekodieren, Ton und Untertitel gar nicht erst anfassen.
    playbin.set_property_from_str("flags", "video");
    let bus = playbin.bus().ok_or_else(|| err(path, "kein Bus"))?;

    let result = (|| {
        playbin
            .set_state(gst::State::Paused)
            .map_err(|e| err(path, e))?;
        wait_async_done(&bus, path)?;
        let at = playbin
            .query_duration::<gst::ClockTime>()
            .map(|d| (d / 10).min(gst::ClockTime::SECOND))
            .unwrap_or(gst::ClockTime::ZERO);
        if at > gst::ClockTime::ZERO
            && playbin
                .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, at)
                .is_ok()
        {
            wait_async_done(&bus, path)?;
        }
        let sample = appsink
            .try_pull_preroll(gst::ClockTime::from_seconds(5))
            .ok_or_else(|| err(path, "kein Bild bekommen"))?;
        let s = sample
            .caps()
            .and_then(|c| c.structure(0).map(|s| s.to_owned()))
            .ok_or_else(|| err(path, "keine Caps"))?;
        let w = s.get::<i32>("width").map_err(|e| err(path, e))? as u32;
        let h = s.get::<i32>("height").map_err(|e| err(path, e))? as u32;
        let buffer = sample.buffer().ok_or_else(|| err(path, "leerer Puffer"))?;
        let map = buffer.map_readable().map_err(|e| err(path, e))?;
        // RGBA: Zeilen sind ohnehin auf 4 Byte ausgerichtet, also dicht gepackt.
        let img = RgbaImage::from_raw(w, h, map.as_slice().to_vec())
            .ok_or_else(|| err(path, "Puffer passt nicht zu den Maßen"))?;
        let img = DynamicImage::ImageRgba8(img);
        Ok(if w.max(h) > max_edge {
            img.thumbnail(max_edge, max_edge)
        } else {
            img
        })
    })();
    let _ = playbin.set_state(gst::State::Null);
    result
}

// ---------------------------------------------------------------- Player

/// Woher das Video kommt. Immich bekommt den Schlüssel als Header, nicht in der URL.
pub enum Quelle {
    Datei(std::path::PathBuf),
    Http { url: String, api_key: String },
}

/// `playbin` mit `gtk4paintablesink`. GtkMediaFile reicht nur lokale Dateien
/// direkt an GStreamer durch; bei HTTP liest es selbst und scheitert.
struct Wiedergabe {
    playbin: gst::Element,
    _bus: gst::bus::BusWatchGuard,
}

type ErrorCallback = Box<dyn Fn(String)>;

pub struct VideoView {
    pub widget: gtk::Overlay,
    picture: gtk::Picture,
    play: gtk::Button,
    mute: gtk::Button,
    volume: gtk::Scale,
    aktiv: RefCell<Option<Wiedergabe>>,
    playing: Cell<bool>,
    muted: Cell<bool>,
    /// Lautstärke merkt sich Blick über Videos hinweg, stumm startet trotzdem jedes.
    level: Cell<f64>,
    on_error: RefCell<Option<ErrorCallback>>,
}

fn flat_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .build();
    b.add_css_class("flat");
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    b
}

fn gst_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(e) = gst::init() {
            tracing::warn!("GStreamer: {e}");
        }
    });
}

impl VideoView {
    pub fn new() -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .hexpand(true)
            .vexpand(true)
            .build();
        let play = flat_button("media-playback-start-symbolic", "Abspielen (Leertaste)");
        let mute = flat_button("audio-volume-muted-symbolic", "Ton an (M)");
        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
        volume.set_draw_value(false);
        volume.set_width_request(120);
        volume.set_value(VOLUME_DEFAULT);
        volume.update_property(&[gtk::accessible::Property::Label("Lautstärke")]);
        volume.set_tooltip_text(Some("Lautstärke (Pfeil hoch/runter)"));
        // Schwebende Leiste wie die Zoomknöpfe: `toolbar` + `osd`.
        let bar = gtk::Box::builder()
            .css_classes(["toolbar", "osd"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .spacing(6)
            .build();
        bar.append(&play);
        bar.append(&mute);
        bar.append(&volume);

        let widget = gtk::Overlay::new();
        widget.set_child(Some(&picture));
        widget.add_overlay(&bar);

        let this = Rc::new(Self {
            widget,
            picture,
            play,
            mute,
            volume,
            aktiv: RefCell::new(None),
            playing: Cell::new(false),
            muted: Cell::new(true),
            level: Cell::new(VOLUME_DEFAULT),
            on_error: RefCell::new(None),
        });
        this.play.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.toggle_play()
        ));
        this.mute.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.toggle_mute()
        ));
        this.volume.connect_value_changed(glib::clone!(
            #[weak]
            this,
            move |s| {
                this.level.set(s.value());
                // Wer am Regler dreht, will etwas hören.
                if s.value() > 0.0 {
                    this.muted.set(false);
                }
                this.apply_volume();
                this.sync();
            }
        ));
        // Klick ins Bild: Abspielen/Anhalten, wie in jedem Player.
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak]
            this,
            move |g, n, _, _| {
                if n == 1 {
                    this.toggle_play();
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        ));
        this.picture.add_controller(click);
        this
    }

    pub fn connect_error(&self, f: impl Fn(String) + 'static) {
        self.on_error.replace(Some(Box::new(f)));
    }

    fn error(&self, msg: String) {
        if let Some(f) = self.on_error.borrow().as_ref() {
            f(msg);
        }
    }

    /// Lädt und startet das Video, stumm.
    pub fn open(self: &Rc<Self>, quelle: Quelle, label: &str) {
        self.stop();
        self.picture
            .update_property(&[gtk::accessible::Property::Label(label)]);
        match self.build(quelle) {
            Ok(w) => {
                self.aktiv.replace(Some(w));
                self.muted.set(true);
                self.apply_volume();
                self.set_playing(true);
            }
            Err(e) => self.error(e),
        }
    }

    fn build(self: &Rc<Self>, quelle: Quelle) -> std::result::Result<Wiedergabe, String> {
        gst_init();
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|_| format!("GStreamer-Element {name} fehlt"))
        };
        let sink = make("gtk4paintablesink")?;
        let paintable = sink.property::<gdk::Paintable>("paintable");
        self.picture.set_paintable(Some(&paintable));
        // Kein video-filter: Die Drehung aus dem Tag setzt der Sink selbst um.
        // Ein videoflip davor zwingt Systemspeicher und bricht unter Wayland
        // die Aushandlung mit GL/DMABuf (not-negotiated).
        let playbin = make("playbin")?;
        playbin.set_property("video-sink", &sink);
        // Keine Untertitel: Bild und Ton, Lautstärke in Software.
        playbin.set_property_from_str("flags", "video+audio+soft-volume");
        let (uri, key) = match quelle {
            Quelle::Datei(p) => (gio::File::for_path(p).uri().to_string(), None),
            Quelle::Http { url, api_key } => (url, Some(api_key)),
        };
        playbin.set_property("uri", uri.as_str());
        if let Some(key) = key {
            playbin.connect("source-setup", false, move |args| {
                if let Ok(src) = args[1].get::<gst::Element>()
                    && src.has_property("extra-headers")
                {
                    let h = gst::Structure::builder("headers")
                        .field("x-api-key", key.as_str())
                        .build();
                    src.set_property("extra-headers", h);
                }
                None
            });
        }
        let bus = playbin.bus().ok_or("kein GStreamer-Bus")?;
        let weak = Rc::downgrade(self);
        let pb = playbin.downgrade();
        let watch = bus
            .add_watch_local(move |_, msg| {
                let (Some(this), Some(pb)) = (weak.upgrade(), pb.upgrade()) else {
                    return glib::ControlFlow::Break;
                };
                match msg.view() {
                    // Kurze Clips laufen in Schleife, wie in Fotos-Apps üblich.
                    gst::MessageView::Eos(_) => {
                        let _ = pb.seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                    }
                    gst::MessageView::Error(e) => {
                        this.playing.set(false);
                        this.sync();
                        this.error(e.error().message().to_owned());
                    }
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| e.to_string())?;
        Ok(Wiedergabe {
            playbin,
            _bus: watch,
        })
    }

    /// Anhalten und Datei freigeben (beim Blättern, Zurückgehen).
    pub fn stop(&self) {
        if let Some(w) = self.aktiv.take() {
            let _ = w.playbin.set_state(gst::State::Null);
        }
        self.playing.set(false);
        self.picture.set_paintable(None::<&gdk::Paintable>);
    }

    pub fn is_open(&self) -> bool {
        self.aktiv.borrow().is_some()
    }

    fn set_playing(&self, on: bool) {
        if let Some(w) = self.aktiv.borrow().as_ref() {
            let state = if on {
                gst::State::Playing
            } else {
                gst::State::Paused
            };
            if w.playbin.set_state(state).is_ok() {
                self.playing.set(on);
            }
        }
        self.sync();
    }

    pub fn toggle_play(&self) {
        self.set_playing(!self.playing.get());
    }

    pub fn toggle_mute(&self) {
        self.muted.set(!self.muted.get());
        self.apply_volume();
        self.sync();
    }

    /// Lauter (+1) oder leiser (−1); hebt die Stummschaltung auf.
    pub fn step_volume(&self, dir: f64) {
        let v = (self.volume.value() + dir * VOLUME_STEP).clamp(0.0, 1.0);
        self.volume.set_value(v);
    }

    fn apply_volume(&self) {
        if let Some(w) = self.aktiv.borrow().as_ref() {
            w.playbin.set_property("mute", self.muted.get());
            // Kubisch, damit der Regler sich gleichmäßig anfühlt.
            w.playbin.set_property("volume", self.level.get().powi(3));
        }
    }

    /// Kurzinfo für die Infotafel: Dauer und Maße, sobald GStreamer sie kennt.
    pub fn info_rows(&self) -> Vec<(&'static str, String)> {
        let Some((playbin, paintable)) = self
            .aktiv
            .borrow()
            .as_ref()
            .map(|w| (w.playbin.clone(), self.picture.paintable()))
        else {
            return Vec::new();
        };
        let mut v = Vec::new();
        if let Some(d) = playbin.query_duration::<gst::ClockTime>() {
            let s = d.seconds();
            v.push((
                "Dauer",
                if s >= 3600 {
                    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
                } else {
                    format!("{}:{:02}", s / 60, s % 60)
                },
            ));
        }
        if let Some(p) = paintable {
            let (w, h) = (p.intrinsic_width(), p.intrinsic_height());
            if w > 0 && h > 0 {
                v.push(("Maße", format!("{w} × {h}")));
            }
        }
        let audio = playbin.property::<i32>("n-audio") > 0;
        v.push(("Ton", if audio { "ja" } else { "keiner" }.to_owned()));
        v
    }

    fn sync(&self) {
        let (icon, tip) = if self.playing.get() {
            ("media-playback-pause-symbolic", "Anhalten (Leertaste)")
        } else {
            ("media-playback-start-symbolic", "Abspielen (Leertaste)")
        };
        self.play.set_icon_name(icon);
        self.play.set_tooltip_text(Some(tip));
        self.play
            .update_property(&[gtk::accessible::Property::Label(tip)]);
        let level = self.level.get();
        let (icon, tip) = if self.muted.get() || level <= 0.0 {
            ("audio-volume-muted-symbolic", "Ton an (M)")
        } else if level < 0.34 {
            ("audio-volume-low-symbolic", "Stumm (M)")
        } else if level < 0.67 {
            ("audio-volume-medium-symbolic", "Stumm (M)")
        } else {
            ("audio-volume-high-symbolic", "Stumm (M)")
        };
        self.mute.set_icon_name(icon);
        self.mute.set_tooltip_text(Some(tip));
        self.mute
            .update_property(&[gtk::accessible::Property::Label(tip)]);
    }
}
