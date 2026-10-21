fn main() {
    glib_build_tools::compile_resources(
        &["../../data"],
        "../../data/resources.gresource.xml",
        "blick.gresource",
    );

    // Für `cargo run` ohne Installation: Schema ins OUT_DIR kompilieren.
    // Installiert (Flatpak) liegt es im Systempfad und wird dort gefunden.
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("schemas");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::copy(
        "../../data/io.github.paulhoever.Blick.gschema.xml",
        out.join("io.github.paulhoever.Blick.gschema.xml"),
    )
    .unwrap();
    let status = std::process::Command::new("glib-compile-schemas")
        .arg(&out)
        .status()
        .expect("glib-compile-schemas fehlt");
    assert!(status.success(), "glib-compile-schemas ist fehlgeschlagen");
    println!("cargo:rerun-if-changed=../../data");
}
