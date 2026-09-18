// Compiles the original C pixel core (copied unchanged from the macOS app's Rendering/ directory) into a
// static library the Rust side binds to in src/ffi.rs.
fn main() {
    let files = [
        "AdjustPixels", "BrushPixels", "ContentFill", "HealPixels",
        "LensPixels", "LevelsPixels", "NoisePixels", "WandPixels",
    ];
    let mut build = cc::Build::new();
    for name in files {
        build.file(format!("csrc/{name}.c"));
        println!("cargo:rerun-if-changed=csrc/{name}.c");
        println!("cargo:rerun-if-changed=csrc/{name}.h");
    }
    build.opt_level(2).compile("compositor_core");
    println!("cargo:rustc-link-lib=m");
}
