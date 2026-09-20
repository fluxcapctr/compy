//! Regressions for the second bug review (REVIEW_RESULTS_2.md).

mod common;

use common::*;
use compositor::brush::BrushSettings;
use compositor::document::{Document, StrokeKind};
use compositor::filters::{self, Adjustment};
use compositor::raster::{new_argb, with_bytes, with_bytes_raw_mut};
use compositor::{format, psd};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn fixture(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/psd").join(name) }
fn temp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("compositor-test-{}-{name}", std::process::id())) }

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in bytes { c ^= b as u32; for _ in 0..8 { c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 }; } }
    !c
}
fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// 2. An image whose header claims 900 megapixels is refused before any pixel buffer is made.
#[test]
fn oversized_image_header_is_refused_before_decoding() {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&30_000u32.to_be_bytes());
    ihdr.extend_from_slice(&30_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", &[0x78, 0x01, 0x01, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x00, 0x01, 0x00, 0x01]));
    png.extend(chunk(b"IEND", &[]));
    let path = temp("huge.png");
    std::fs::write(&path, &png).unwrap();
    let started = Instant::now();
    let message = match Document::open_image(&path) { Err(e) => e.to_string(), Ok(_) => panic!("opened") };
    assert!(message.contains("30000 x 30000"), "{message}");
    assert!(started.elapsed().as_millis() < 500, "refused from the header");
    let _ = std::fs::remove_file(&path);
}

/// 3. A brush on a layer squeezed to a sliver never allocates a tip thousands of pixels wide: the
/// layer is first rasterized to the pixels it shows (square, at canvas scale), then painted.
#[test]
fn brush_tip_is_bounded() {
    let mut f = Fixture::new("review2-tip", 1, 1);
    let id = f.add(Spec { size: (1.0, 1.0), pixels: Some(solid(10_000, 1, RED)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.begin_stroke((0.5, 0.5), &BrushSettings { diameter: 2000.0, ..Default::default() }, StrokeKind::Paint).unwrap();
    assert_eq!(d.renderer.image_size(id), Some((1, 1)), "the sliver became the one pixel it shows");
    assert!(d.stroke_active());
    d.cancel_stroke();
    assert!(!d.stroke_active());
    // A brush within the limit on a less extreme layer still works.
    let mut f = Fixture::new("review2-tip-ok", 4, 4);
    let id = f.add(Spec { size: (4.0, 4.0), pixels: Some(solid(400, 400, RED)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.begin_stroke((2.0, 2.0), &BrushSettings { diameter: 2.0, ..Default::default() }, StrokeKind::Paint).unwrap();
    d.finish_stroke().unwrap();
}

/// 4 and 5. Channel skips and Unicode names stay inside the file.
#[test]
fn psd_skips_and_names_are_bounded() {
    for name in ["skip-past-eof.psd", "unicode-count.psd"] {
        let started = Instant::now();
        assert!(psd::read(&fixture(name)).is_err(), "{name} was accepted");
        assert!(started.elapsed().as_millis() < 500, "{name} took too long");
    }
}

/// 6. Adjusting a layer that no longer exists is a no-op, not a panic.
#[test]
fn adjustment_edits_survive_a_deleted_layer() {
    let mut d = Document::blank(4, 4, 72.0).unwrap();
    let id = d.add_adjustment("Exposure").unwrap();
    assert!(d.adjustment(id).is_some());
    d.delete_layer();
    assert!(!d.has_layer(id));
    assert!(d.adjustment(id).is_none());
    assert!(!d.set_adjustment(id, &Adjustment::Exposure(filters::Exposure { exposure: 1.0, offset: 0.0, gamma: 1.0 }), false));
    assert!(!d.set_adjustment(id, &Adjustment::Exposure(filters::Exposure { exposure: 1.0, offset: 0.0, gamma: 1.0 }), true));
}

/// 7. Visibility, opacity and blend mode are edits: they mark the document and undo, and slider steps merge.
#[test]
fn appearance_changes_are_edits() {
    let mut d = Document::blank(4, 4, 72.0).unwrap();
    let id = d.active.unwrap();
    assert!(!d.is_modified());
    d.set_opacity(id, 0.8);
    d.set_opacity(id, 0.6);
    d.set_opacity(id, 0.5);
    assert!(d.is_modified());
    assert_eq!(d.renderer.layer(id).opacity(), 0.5);
    assert_eq!(d.undo_name(), Some("Opacity"));
    d.undo();
    assert_eq!(d.renderer.layer(id).opacity(), 1.0, "the slider's steps undo as one");
    assert!(!d.is_modified());
    d.set_visible(id, false);
    assert!(!d.renderer.layer(id).is_visible);
    assert_eq!(d.undo_name(), Some("Hide Layer"));
    d.undo();
    assert!(d.renderer.layer(id).is_visible);
    d.set_blend_mode(id, format::BlendMode::Multiply);
    assert_eq!(d.undo_name(), Some("Blend Mode"));
    d.undo();
    assert_eq!(d.renderer.layer(id).blend_mode(), format::BlendMode::Normal);
}

/// 9. Adjustment settings of the wrong shape are rejected, not defaulted.
#[test]
fn wrongly_shaped_adjustment_settings_are_rejected() {
    let cases = [
        serde_json::json!({"kind": "Levels", "levels": 42}),
        serde_json::json!({"kind": "Levels", "levels": {"ranges": "x"}}),
        serde_json::json!({"kind": "Levels", "levels": {"ranges": [1, 2, 3, 4]}}),
        serde_json::json!({"kind": "Levels", "levels": {"ranges": [{"black": "0"}, {}, {}, {}]}}),
        serde_json::json!({"kind": "Curves", "curves": []}),
        serde_json::json!({"kind": "Exposure", "exposureSettings": {"exposure": "1"}}),
        serde_json::json!({"kind": "Hue/Saturation", "hue": 0, "saturation": 0, "lightness": 0, "colorize": "no"}),
        serde_json::json!({"kind": "Hue/Saturation", "hsvSettings": {"range": "Purples"}}),
        serde_json::json!({"kind": "Hue/Saturation", "hsvSettings": {"adjustments": 7}}),
        serde_json::json!({"kind": "Gradient Map", "gradientMapSettings": {"shadows": [0, 0, 0]}}),
    ];
    for (i, case) in cases.into_iter().enumerate() {
        let mut f = Fixture::new(&format!("review2-shape-{i}"), 1, 1);
        f.add(Spec { adjustment: Some(case), ..Default::default() });
        assert!(f.load().is_err(), "case {i} was accepted");
    }
    // Absent fields still default, as the reader promises.
    let mut f = Fixture::new("review2-shape-ok", 1, 1);
    f.add(Spec { adjustment: Some(serde_json::json!({"kind": "Levels"})), ..Default::default() });
    assert!(f.load().is_ok());
}

/// Raw pixel access refuses to alias: readers share, a writer is alone.
#[test]
fn raw_pixel_access_refuses_aliasing() {
    let surface = new_argb(4, 4).unwrap();
    let nested_write = with_bytes(&surface, |_, _| with_bytes_raw_mut(&surface, |_, _| ()).is_err()).unwrap();
    assert!(nested_write, "a write inside a read is refused");
    let nested_read = with_bytes_raw_mut(&surface, |_, _| with_bytes(&surface, |_, _| ()).is_err()).unwrap();
    assert!(nested_read, "a read inside a write is refused");
    let two_reads = with_bytes(&surface, |_, _| with_bytes(&surface, |_, _| ()).is_ok()).unwrap();
    assert!(two_reads, "readers share");
    // Sequential access is unaffected, and a refused borrow leaves no trace.
    with_bytes_raw_mut(&surface, |d, _| d[0] = 7).unwrap();
    assert_eq!(with_bytes(&surface, |d, _| d[0]).unwrap(), 7);
    with_bytes_raw_mut(&surface, |d, _| d[0] = 9).unwrap();
}
