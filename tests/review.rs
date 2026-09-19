//! Regressions for the 2026-09-18 bug review (REVIEW_RESULTS.md), one per finding.

mod common;

use common::*;
use compositor::brush::BrushSettings;
use compositor::document::{Document, StrokeKind};
use compositor::filters::{self, Adjustment};
use compositor::history::History;
use compositor::{format, psd};
use std::path::{Path, PathBuf};

fn flat(d: &mut Document) -> Vec<u8> {
    let s = d.renderer.render_flat().unwrap();
    compositor::raster::with_bytes(&s, |b, _| b.to_vec()).unwrap()
}
fn opaque_count(pixels: &[u8]) -> usize { pixels.chunks(4).filter(|p| p[3] > 0).count() }
fn fixture(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/psd").join(name) }

/// 1. Pixels painted onto a blank layer survive a save and reopen.
#[test]
fn painted_blank_layer_saves() {
    let mut d = Document::blank(16, 16, 72.0).unwrap();
    d.begin_stroke((8.0, 8.0), &BrushSettings { diameter: 4.0, color: [1.0, 0.0, 0.0], ..Default::default() }, StrokeKind::Paint).unwrap();
    d.finish_stroke().unwrap();
    let before = opaque_count(&flat(&mut d));
    assert!(before > 0);
    assert!(d.renderer.layers()[0].image_file.is_some(), "the record names its image file");
    let path = std::env::temp_dir().join(format!("compositor-test-{}-painted.comp", std::process::id()));
    d.save(&path).unwrap();
    let mut reopened = Document::new(format::load(&path).unwrap()).unwrap();
    assert_eq!(opaque_count(&flat(&mut reopened)), before);
    let _ = std::fs::remove_dir_all(&path);
}

/// 2 and 3. Malformed PSDs are refused with an error, never a panic or a huge allocation.
#[test]
fn malformed_psds_are_refused() {
    for name in ["missing-channels.psd", "overflow-rectangle.psd", "unbounded-channel.psd"] {
        let result = psd::read(&fixture(name));
        assert!(result.is_err(), "{name} was accepted");
    }
    // A layer whose channel claims more bytes than the file has.
    let mut bytes = std::fs::read(fixture("unbounded-channel.psd")).unwrap();
    bytes.truncate(60);
    let path = std::env::temp_dir().join(format!("compositor-test-{}-truncated.psd", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    assert!(psd::read(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

/// 4. Hue/Saturation records in the Swift app's shape (enum-keyed dictionaries as flat arrays) decode and apply.
#[test]
fn swift_hue_saturation_records_decode() {
    let value = serde_json::json!({"kind": "Hue/Saturation", "hue": 120, "saturation": 0, "lightness": 0, "colorize": false,
        "hsvSettings": {"range": "Master", "colorize": false, "invertRange": false, "adjustments": ["Master", {"hue": 120, "saturation": 0, "lightness": 0}], "bands": []}});
    let record: format::Adjustment = serde_json::from_value(value).unwrap();
    let adjustment = Adjustment::from_record(&record).unwrap();
    let mut px = [0, 0, 255, 255];
    adjustment.apply(&mut px, 1, 1, (0.0, 0.0), 1.0);
    assert!(px[1] > 200 && px[2] < 50, "red shifted 120 degrees is green: {px:?}");
    // What this app writes is the same shape, and reads back to the same adjustment.
    let written = adjustment.to_record();
    assert!(written.settings["hsvSettings"]["adjustments"].is_array());
    assert!(written.settings["hsvSettings"]["bands"].is_array());
    let Adjustment::HueSaturation(reread) = Adjustment::from_record(&written).unwrap() else { panic!("kind") };
    assert_eq!(reread.adjustments, vec![("Master".to_string(), [120.0, 0.0, 0.0])]);
    assert!(reread.bands.len() == 7 && reread.bands.iter().all(|(r, b)| *b == filters::default_band(r)));
    // invertRange applies the selected range to everything outside its band.
    let mut inverted = filters::HueSaturation { range: "Reds".into(), invert_range: true, ..Default::default() };
    inverted.set_adjustment("Reds", [0.0, -100.0, 0.0]);
    let mut blue = [255, 0, 0, 255];
    Adjustment::HueSaturation(inverted.clone()).apply(&mut blue, 1, 1, (0.0, 0.0), 1.0);
    assert!(blue[0] == blue[1] && blue[1] == blue[2], "outside Reds, inverted, blue is desaturated: {blue:?}");
    let mut red = [0, 0, 255, 255];
    Adjustment::HueSaturation(inverted).apply(&mut red, 1, 1, (0.0, 0.0), 1.0);
    assert_eq!(red, [0, 0, 255, 255], "inside Reds, inverted, red is untouched");
}

/// 5. Undoing a move restores the mask's placement in the render, not just the record.
#[test]
fn undo_restores_placed_masks() {
    let mut f = Fixture::new("review-undo", 12, 4);
    let id = f.add(Spec { size: (8.0, 4.0), pixels: Some(solid(8, 4, RED)), mask: Some((8, 4, (0..32).map(|i| if i % 8 >= 4 { 255 } else { 0 }).collect())), mask_placement: Some(((1.0, 0.0), (8.0, 4.0))), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.renderer.set_mask_linked(id, false);
    let before = flat(&mut d);
    d.nudge(2.0, 0.0);
    let moved = flat(&mut d);
    assert_ne!(before, moved);
    d.undo();
    assert_eq!(before, flat(&mut d));
    d.redo();
    assert_eq!(moved, flat(&mut d));
}

/// 6. Clone Stamp composites the sample over the layer: a transparent sample changes nothing.
#[test]
fn clone_stamp_from_transparency_keeps_pixels() {
    let mut f = Fixture::new("review-clone", 8, 4);
    let id = f.add(Spec { size: (8.0, 4.0), pixels: Some(halves(8, 4, 4, CLEAR, BLUE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.begin_stroke((6.0, 2.0), &BrushSettings { diameter: 2.0, ..Default::default() }, StrokeKind::Clone { offset: (-4.0, 0.0), all_layers: false }).unwrap();
    d.finish_stroke().unwrap();
    let b = flat(&mut d);
    assert_eq!(&b[(2 * 8 + 6) * 4..(2 * 8 + 6) * 4 + 4], &[255, 0, 0, 255]);
    // Cloning opaque red onto blue still paints it.
    let mut f = Fixture::new("review-clone-2", 8, 4);
    let id = f.add(Spec { size: (8.0, 4.0), pixels: Some(halves(8, 4, 4, RED, BLUE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.begin_stroke((6.0, 2.0), &BrushSettings { diameter: 2.0, ..Default::default() }, StrokeKind::Clone { offset: (-4.0, 0.0), all_layers: false }).unwrap();
    d.finish_stroke().unwrap();
    let b = flat(&mut d);
    let p = &b[(2 * 8 + 6) * 4..(2 * 8 + 6) * 4 + 4];
    assert!(p[2] > 100 && p[0] < 155, "red cloned over blue: {p:?}");
}

/// 7. An adjustment layer's mask limits it to the mask's rectangle.
#[test]
fn adjustment_mask_stops_at_its_rectangle() {
    let mut f = Fixture::new("review-adjust-mask", 8, 4);
    f.add(Spec { size: (8.0, 4.0), pixels: Some(solid(8, 4, [192, 192, 192, 255])), ..Default::default() });
    let mut l = filters::Levels::default();
    l.ranges[0].black = 128.0;
    f.add(Spec { size: (2.0, 4.0), mask: Some((1, 1, vec![255])), adjustment: Some(serde_json::to_value(Adjustment::Levels(l).to_record()).unwrap()), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    let b = flat(&mut d);
    assert_eq!(&b[(2 * 8 + 6) * 4..(2 * 8 + 6) * 4 + 4], &[192, 192, 192, 255], "outside the mask");
    assert!(b[(2 * 8 + 1) * 4] < 140, "inside the mask the levels apply: {}", b[(2 * 8 + 1) * 4]);
}

/// 8. A failed Image Size leaves history usable and the document as it was.
#[test]
fn failed_image_size_rolls_back() {
    let mut f = Fixture::new("review-resize-error", 1, 1);
    let id = f.add(Spec { size: (30000.0, 1.0), pixels: Some(solid(1, 1, RED)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.rename_layer(id, "Renamed");
    assert!(d.can_undo());
    assert!(d.image_size(2, 1, 72.0, format::Sampling::Nearest).is_err());
    assert!(d.can_undo(), "undo still works after the failure");
    assert_eq!(d.undo_name(), Some("Rename Layer"));
    assert_eq!(d.width(), 1);
    assert_eq!(d.renderer.layer(id).transform.size.0, 30000.0);
}

/// 9. Adjustment settings outside the reference's bounds are rejected on load.
#[test]
fn invalid_adjustment_settings_are_rejected() {
    let cases = [
        serde_json::json!({"kind": "Curves", "curves": {"channels": [[{"x": 0, "y": 0}, {"x": 0, "y": 255}]]}}),
        serde_json::json!({"kind": "Curves", "curves": {"channels": [[{"x": 0, "y": 0}, {"x": 100, "y": 50}, {"x": 50, "y": 60}, {"x": 255, "y": 255}], [{"x": 0, "y": 0}, {"x": 255, "y": 255}], [{"x": 0, "y": 0}, {"x": 255, "y": 255}], [{"x": 0, "y": 0}, {"x": 255, "y": 255}]]}}),
        serde_json::json!({"kind": "Levels", "levels": {"channel": "RGB", "ranges": [{"black": 300, "gamma": 1, "white": 255, "outputBlack": 0, "outputWhite": 255}]}}),
        serde_json::json!({"kind": "Exposure", "exposureSettings": {"exposure": 40, "offset": 0, "gamma": 1}}),
        serde_json::json!({"kind": "Hue/Saturation", "hue": 720, "saturation": 0, "lightness": 0, "colorize": false}),
        serde_json::json!({"kind": "Gradient Map", "gradientMapSettings": {"shadows": {"red": 2, "green": 0, "blue": 0}, "highlights": {"red": 1, "green": 1, "blue": 1}, "reversed": false}}),
        serde_json::json!({"kind": "Grain", "grainSettings": {"amount": 25, "size": 0, "roughness": 50, "seed": 1}}),
    ];
    for (i, case) in cases.into_iter().enumerate() {
        let mut f = Fixture::new(&format!("review-validation-{i}"), 1, 1);
        f.add(Spec { adjustment: Some(case), ..Default::default() });
        assert!(f.load().is_err(), "case {i} was accepted");
    }
    // Sound settings still load, including what this app writes.
    let mut f = Fixture::new("review-validation-ok", 1, 1);
    let mut l = filters::Levels::default();
    l.ranges[0].black = 20.0;
    f.add(Spec { adjustment: Some(serde_json::to_value(Adjustment::Levels(l).to_record()).unwrap()), ..Default::default() });
    let mut hs = filters::HueSaturation::default();
    hs.set_adjustment("Reds", [30.0, 10.0, 0.0]);
    f.add(Spec { adjustment: Some(serde_json::to_value(Adjustment::HueSaturation(hs).to_record()).unwrap()), ..Default::default() });
    assert!(f.load().is_ok());
}

/// 10. Undoing back to the saved state reads as unmodified, and so does redoing back to it.
#[test]
fn undo_to_saved_state_is_unmodified() {
    let mut h = History::new(10, 100);
    h.begin("edit", 0); h.end(1, |_, _| 0);
    assert!(h.is_modified());
    h.undo();
    assert!(!h.is_modified(), "back at the saved state");
    h.redo();
    assert!(h.is_modified());
    h.mark_saved();
    h.undo();
    assert!(h.is_modified());
    h.redo();
    assert!(!h.is_modified(), "redo returns to the saved state");
    h.begin("edit", 1); h.end(2, |_, _| 0);
    h.undo();
    assert!(!h.is_modified());
}
