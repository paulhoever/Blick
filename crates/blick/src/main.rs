//! Blick: Bildbetrachter mit Kontaktbogen, Korrekturen, Batch-Rotation und Immich.

mod config;
mod convert;
mod crop_view;
mod dupes_view;
mod ctx;
mod edit;
mod glycin_loader;
mod library_view;
mod looks;
mod photo;
mod prefs;
mod runtime;
mod sheet;
mod shortcuts;
mod theme;
mod tile;
mod upload;
mod video;
mod viewer;
mod window;
mod zoom;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BLICK_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();

    blick_core::decode::set_fallback(glycin_loader::load_blocking);
    blick_core::look::set_resolver(looks::resolve_key);
    blick_core::rotate::set_fallback(glycin_loader::rotate_blocking);
    blick_core::decode::set_frame_grabber(video::grab_frame);

    // `blick --probe <dateien>`: nur dekodieren und Maße ausgeben, ohne Fenster.
    // Dient zum Testen der Loader im Flatpak.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--probe") {
        return probe(&args[2..]);
    }
    // `blick --rotate <links|rechts> <dateien>`: drehen ohne Fenster, zum Testen.
    if args.get(1).map(String::as_str) == Some("--rotate") && args.len() >= 4 {
        let rot = if args[2] == "links" {
            blick_core::Rotation::Left
        } else {
            blick_core::Rotation::Right
        };
        let mut ok = true;
        for f in &args[3..] {
            match blick_core::rotate::rotate_file(std::path::Path::new(f), rot) {
                Ok(m) => println!("{f}: {m:?}"),
                Err(e) => {
                    ok = false;
                    println!("{f}: FEHLER {e}");
                }
            }
        }
        return if ok {
            glib::ExitCode::SUCCESS
        } else {
            glib::ExitCode::FAILURE
        };
    }
    // `blick --apply-look <stil> <bild> <ziel>`: Stil anwenden, ohne Fenster.
    if args.get(1).map(String::as_str) == Some("--apply-look") && args.len() == 5 {
        return apply_look(&args[2], &args[3], &args[4]);
    }

    gio::resources_register_include!("blick.gresource").expect("Ressourcen fehlen im Binary");

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        theme::apply(&config::settings());
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.shortcuts", &["<Control>question"]);
        app.set_accels_for_action("win.open-folder", &["<Control>o"]);
        app.set_accels_for_action("win.rotate-right", &["<Control>r"]);
        app.set_accels_for_action("win.rotate-left", &["<Control><Shift>r"]);
        app.set_accels_for_action("win.upload", &["<Control>u"]);
        app.set_accels_for_action("win.find-duplicates", &["<Control><Shift>d"]);
        app.set_accels_for_action("win.fullscreen", &["F11"]);
        app.set_accels_for_action("win.trash", &["Delete"]);
        app.set_accels_for_action("win.info", &["<Alt>Return"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);

        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &adw::Application, _, _| app.quit())
            .build();
        app.add_action_entries([quit]);
    });

    // `blick -r ~/Photos`: Ordner rekursiv als Bibliothek aufnehmen.
    let recursive = args.iter().any(|a| a == "-r" || a == "--recursive");
    let args: Vec<String> = args
        .into_iter()
        .filter(|a| a != "-r" && a != "--recursive")
        .collect();

    app.connect_activate(|app| {
        window::Window::open(app, None, Vec::new(), false).present();
    });

    // `blick ~/Bilder/Urlaub` öffnet direkt dort.
    app.connect_open(move |app, files, _| {
        let paths: Vec<_> = files.iter().filter_map(|f| f.path()).collect();
        // Ein Ordner als Argument öffnet den Kontaktbogen, Bilddateien gehen direkt in den Viewer.
        let (folder, images): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_dir());
        let start = folder.into_iter().next();
        let images = if start.is_some() { Vec::new() } else { images };
        window::Window::open(app, start, images, recursive).present();
    });

    app.run_with_args(&args)
}

fn probe(files: &[String]) -> glib::ExitCode {
    let mut failed = false;
    for f in files {
        let t = std::time::Instant::now();
        match blick_core::decode::load_preview(std::path::Path::new(f), 4096, 4096) {
            Ok(px) => println!("{f}: {}x{} in {:?}", px.width, px.height, t.elapsed()),
            Err(e) => {
                failed = true;
                println!("{f}: FEHLER {e}");
            }
        }
    }
    if failed {
        glib::ExitCode::FAILURE
    } else {
        glib::ExitCode::SUCCESS
    }
}

fn apply_look(look: &str, src: &str, dst: &str) -> glib::ExitCode {
    let run = || -> anyhow::Result<()> {
        let t = std::time::Instant::now();
        let look = blick_core::look::Look::load(std::path::Path::new(look))?;
        let render = look.render(1.0);
        let baked = t.elapsed();
        blick_core::export::save_adjusted(
            std::path::Path::new(src),
            std::path::Path::new(dst),
            None,
            blick_core::Adjustments::default(),
            Some(&render),
        )?;
        println!(
            "{}: gebacken in {baked:?}, gesamt {:?}",
            look.name,
            t.elapsed()
        );
        if !look.ignored.is_empty() {
            println!("nicht übernommen: {}", look.ignored.join(", "));
        }
        Ok(())
    };
    match run() {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            glib::ExitCode::FAILURE
        }
    }
}
