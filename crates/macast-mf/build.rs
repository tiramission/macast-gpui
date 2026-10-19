fn main() {
    // Tell the linker where to find mpv.lib (the libmpv import library).
    // The .lib file is checked in alongside this build script.
    println!("cargo:rustc-link-search=native={}", env!("CARGO_MANIFEST_DIR"));
}
