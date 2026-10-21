//! Blick: image viewer with contact sheet, adjustments, batch rotation and Immich.

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

    // `blick --probe <files>`: only decode and print dimensions, no window.
    // Used to test the loaders in the Flatpak.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--probe") {
        return probe(&args[2..]);
    }
    // `blick --rotate <left|right> <files>`: rotate without a window, for testing.
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
    // `blick --apply-look <look> <image> <target>`: apply a look, no window.
    if args.get(1).map(String::as_str) == Some("--apply-look") && args.len() == 5 {
        return apply_look(&args[2], &args[3], &args[4]);
    }

    gio::resources_register_include!("blick.gresource").expect("resources missing from the binary");

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

    // `blick -r ~/Photos`: add a folder to the library, recursively.
    let recursive = args.iter().any(|a| a == "-r" || a == "--recursive");
    let args: Vec<String> = args
        .into_iter()
        .filter(|a| a != "-r" && a != "--recursive")
        .collect();

    app.connect_activate(|app| {
        window::Window::open(app, None, Vec::new(), false).present();
    });

    // `blick ~/Pictures/Holiday` opens right there.
    app.connect_open(move |app, files, _| {
        let paths: Vec<_> = files.iter().filter_map(|f| f.path()).collect();
        // A folder argument opens the contact sheet, image files go straight to the viewer.
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
            println!("not applied: {}", look.ignored.join(", "));
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
