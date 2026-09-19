//! A real Photoshop 7 brush file (from Krita's test data) loads, and its tips look like brush tips.

use std::path::Path;

#[test]
fn a_real_abr_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-brushes.abr");
    let presets = compositor::abr::load(&path).unwrap();
    assert!(presets.len() >= 5, "{} presets", presets.len());
    // For a look: the first few tips as PNGs beside the temp dir when asked.
    if let Ok(dir) = std::env::var("ABR_DUMP") {
        for (i, p) in presets.iter().take(6).enumerate() {
            let rgba: Vec<u8> = p.pixels.iter().flat_map(|v| [255 - v, 255 - v, 255 - v, 255]).collect();
            let surface = compositor::png_io::from_straight_rgba(&rgba, p.width, p.height).unwrap();
            compositor::png_io::encode(&surface, Path::new(&dir).join(format!("tip-{i}.png")).as_path(), 72.0).unwrap();
        }
    }
    for p in &presets {
        assert!(p.width > 8 && p.height > 8 && p.pixels.len() == p.width * p.height, "{}: {} x {}", p.name, p.width, p.height);
        let painted = p.pixels.iter().filter(|v| **v > 32).count();
        let ratio = painted as f64 / p.pixels.len() as f64;
        let max = p.pixels.iter().copied().max().unwrap_or(0);
        eprintln!("{}: {} x {} spacing {} painted {:.1}% max {max}", p.name, p.width, p.height, p.spacing, ratio * 100.0);
        assert!(max > 64, "{}: an empty tip", p.name);
    }
    eprintln!("{} presets; first: {} {}x{} spacing {}", presets.len(), presets[0].name, presets[0].width, presets[0].height, presets[0].spacing);
}
