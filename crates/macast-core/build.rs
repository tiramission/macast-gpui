fn main() {
    // mpv.lib (the libmpv import library, taken from the mpv-dev package) lives
    // next to this build script; tell the linker where to look for it.
    println!(
        "cargo:rustc-link-search=native={}",
        env!("CARGO_MANIFEST_DIR")
    );
}
