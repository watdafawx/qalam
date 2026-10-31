// Embed the app manifest: modern control styles (needed for task dialogs) and per-monitor DPI awareness.
fn main() {
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("qalam.manifest");
    println!("cargo:rerun-if-changed=qalam.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
}
