fn main() {
    glib_build_tools::compile_resources(
        &["../../data"],
        "../../data/resources.gresource.xml",
        "blick.gresource",
    );

    // For `cargo run` without installing: compile the schema into OUT_DIR.
    // Installed (Flatpak), it's in the system path and found there.
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
    assert!(status.success(), "glib-compile-schemas failed");
    println!("cargo:rerun-if-changed=../../data");
}
