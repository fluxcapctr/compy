//! Regressions for the third bug review (REVIEW_RESULTS_3.md).

mod common;

use common::*;
use compositor::brush::BrushSettings;
use compositor::distort;
use compositor::document::{Document, Place, StrokeKind};
use compositor::{abr, matte};
use std::rc::Rc;

fn flat(d: &mut Document) -> Vec<u8> { compositor::raster::with_bytes(&d.renderer.render_flat().unwrap(), |b, _| b.to_vec()).unwrap() }

/// A version 2 ABR with one brush from raw parts.
fn abr_v2(w: i32, h: i32, compression: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![0, 0, 0, 0, 0, 25];
    body.extend_from_slice(&0u32.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&[0; 8]);
    for v in [0i32, 0, h, w] { body.extend_from_slice(&v.to_be_bytes()); }
    body.extend_from_slice(&8i16.to_be_bytes());
    body.push(compression);
    body.extend_from_slice(payload);
    let mut f = vec![0, 2, 0, 1, 0, 2];
    f.extend_from_slice(&(body.len() as u32).to_be_bytes());
    f.extend_from_slice(&body);
    f
}

/// 1. Damaged brush rows and oversized files are refused rather than decoded as blanks or allocated.
#[test]
fn malformed_abr_rows_are_refused() {
    // Two empty packed rows for a 4 x 2 tip: short rows are an error, not a blank brush.
    let empty_rows = abr_v2(4, 2, 1, &[0, 0, 0, 0]);
    assert!(abr::parse(&empty_rows, "x").is_err());
    // A raw tip claiming more pixels than its record holds.
    assert!(abr::parse(&abr_v2(4, 2, 0, &[255; 3]), "x").is_err());
    // Unknown compression.
    assert!(abr::parse(&abr_v2(1, 1, 7, &[0, 0, 0]), "x").is_err());
    // A well-formed packed tip still reads.
    let good = abr_v2(4, 2, 1, &[0, 5, 0, 2, 3, 1, 2, 3, 4, 0xfd, 9]);
    let p = abr::parse(&good, "x").unwrap();
    assert_eq!(p[0].pixels, vec![1, 2, 3, 4, 9, 9, 9, 9]);
    // A stack of huge tips with empty rows exceeds the budget before any allocation.
    let mut many = vec![0, 2, 0, 8];
    for _ in 0..8 {
        let mut body = vec![0, 0, 0, 0, 0, 25];
        body.extend_from_slice(&0u32.to_be_bytes());
        body.push(1);
        body.extend_from_slice(&[0; 8]);
        for v in [0i32, 0, 7000, 7000] { body.extend_from_slice(&v.to_be_bytes()); }
        body.extend_from_slice(&8i16.to_be_bytes());
        body.push(1);
        body.extend_from_slice(&vec![0u8; 14000]);
        many.extend_from_slice(&2u16.to_be_bytes());
        many.extend_from_slice(&(body.len() as u32).to_be_bytes());
        many.extend_from_slice(&body);
    }
    let started = std::time::Instant::now();
    assert!(abr::parse(&many, "x").is_err());
    assert!(started.elapsed().as_millis() < 500);
}

/// 2. Merging a folder together with its child lands the result where the folder was, with a real parent.
#[test]
fn merging_a_folder_with_its_child_keeps_the_tree_valid() {
    let mut f = Fixture::new("merge-folder-child", 8, 8);
    let folder = f.add(Spec { name: "F", group: true, size: (8.0, 8.0), ..Default::default() });
    let child = f.add(Spec { name: "C", size: (8.0, 8.0), pixels: Some(solid(8, 8, RED)), parent: Some(folder), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.select_layer(Some(folder));
    d.toggle_layer_selected(child);
    let before = flat(&mut d);
    d.merge_layers().unwrap();
    assert_eq!(d.renderer.layers().len(), 1);
    assert_eq!(d.renderer.layers()[0].parent_id, None);
    assert_eq!(flat(&mut d), before);
    let json = serde_json::to_vec(&d.manifest()).unwrap();
    assert!(compositor::format::Manifest::parse(&json).is_ok());
}

/// 4. A flipped layer's separately placed mask stays put through an identity distortion.
#[test]
fn distort_carry_ignores_pixel_flips() {
    let mut f = Fixture::new("distort-flip", 8, 8);
    let id = f.add(Spec { size: (8.0, 8.0), flip: (true, false), pixels: Some(solid(8, 8, RED)), mask: Some((2, 8, vec![255; 16])), mask_placement: Some(((1.0, 0.0), (2.0, 8.0))), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    let t = d.renderer.layer(id).transform;
    let before = flat(&mut d);
    let corners = distort::corners(&t);
    let placement = d.renderer.layer(id).mask_placement.unwrap();
    let carried = distort::carried(&placement, &t, &corners).unwrap();
    assert!((carried[0].0 - 1.0).abs() < 1e-9 && carried[0].1.abs() < 1e-9, "identity carry leaves the placement: {carried:?}");
    d.commit_distort(&[(id, t, corners)]).unwrap();
    assert_eq!(flat(&mut d), before);
}

/// 6. Dragging a folder together with its child keeps the child inside it.
#[test]
fn dragging_folder_and_child_keeps_nesting() {
    let mut f = Fixture::new("drag-folder-child", 4, 4);
    let a = f.add(Spec { name: "A", size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let folder = f.add(Spec { name: "F", group: true, size: (4.0, 4.0), ..Default::default() });
    let c = f.add(Spec { name: "C", size: (4.0, 4.0), pixels: Some(solid(4, 4, BLUE)), parent: Some(folder), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.move_layers(&[folder, c], Place::Below(a)).unwrap();
    assert_eq!(d.renderer.layer(c).parent_id, Some(folder));
    assert_eq!(d.renderer.layers()[0].id, folder);
}

/// 7. A jittered stroke replays exactly, provisional tails or not.
#[test]
fn jittered_strokes_replay_exactly() {
    let bar = Rc::new(abr::Preset { name: "bar".into(), width: 8, height: 2, pixels: vec![255; 16], spacing: 50.0, jitter: 1.0, set: "t".into(), frames: vec![vec![128; 16], vec![64; 16]] });
    let settings = BrushSettings { diameter: 14.0, spacing: Some(0.5), angle_jitter: 1.0, preset: Some(bar), ..Default::default() };
    let points = [(20.0, 32.0), (40.0, 32.0), (60.0, 32.0), (90.0, 32.0)];
    let mut f = Fixture::new("jitter-live", 128, 64);
    let id = f.add(Spec { size: (128.0, 64.0), pixels: Some(solid(128, 64, CLEAR)), ..Default::default() });
    let mut live = Document::new(f.load().unwrap()).unwrap();
    live.active = Some(id);
    live.begin_stroke(points[0], &settings, StrokeKind::Paint).unwrap();
    for p in &points[1..] { live.continue_stroke(*p).unwrap(); }
    live.finish_stroke().unwrap();
    let mut g = Fixture::new("jitter-replay", 128, 64);
    let id2 = g.add(Spec { size: (128.0, 64.0), pixels: Some(solid(128, 64, CLEAR)), ..Default::default() });
    let mut replayed = Document::new(g.load().unwrap()).unwrap();
    replayed.active = Some(id2);
    replayed.replay_stroke(&points, &settings, StrokeKind::Paint).unwrap();
    assert_eq!(flat(&mut live), flat(&mut replayed));
}

/// 8. The distortion preview shows the mask it will commit.
#[test]
fn distort_preview_matches_commit_with_a_mask() {
    let mut f = Fixture::new("distort-preview-mask", 16, 8);
    let id = f.add(Spec { size: (8.0, 8.0), pixels: Some(solid(8, 8, RED)), mask: Some((8, 8, (0..64).map(|i| if i % 8 < 4 { 255 } else { 0 }).collect())), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    let t = d.renderer.layer(id).transform;
    let mut corners = distort::corners(&t);
    corners[1] = (16.0, 0.0);
    d.preview_distort(id, &t, &corners).unwrap();
    let preview = flat(&mut d);
    d.commit_distort(&[(id, t, corners)]).unwrap();
    let committed = flat(&mut d);
    let differing = preview.iter().zip(&committed).filter(|(a, b)| a != b).count();
    assert!(differing <= 8, "{differing} bytes differ between the preview and the commit");
    // Cancelling drops both previews.
    d.undo();
    d.preview_distort(id, &t, &corners).unwrap();
    d.cancel_distort(id);
    assert!(d.renderer.mask_preview(id).is_none());
}

/// 9. A file that is not a whole model does not count as one.
#[test]
fn a_partial_model_file_is_not_ready() {
    let path = std::env::temp_dir().join(format!("compositor-test-{}-not-a-model.onnx", std::process::id()));
    std::fs::write(&path, vec![0u8; 1_000_001]).unwrap();
    // The default location demands the exact size; only an explicitly named model is taken on trust.
    temp_env(&[("COMPOSITOR_MODEL", None), ("XDG_DATA_HOME", Some(std::env::temp_dir().join("compositor-test-nowhere").to_str().unwrap()))], || assert!(!matte::model_ready()));
    temp_env(&[("COMPOSITOR_MODEL", Some(path.to_str().unwrap()))], || assert!(matte::model_ready(), "a named model only has to exist"));
    let _ = std::fs::remove_file(&path);
}

fn temp_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
    let saved: Vec<(String, Option<String>)> = vars.iter().map(|(k, _)| (k.to_string(), std::env::var(k).ok())).collect();
    for (k, v) in vars { match v { Some(v) => unsafe { std::env::set_var(k, v) }, None => unsafe { std::env::remove_var(k) } } }
    f();
    for (k, v) in saved { match v { Some(v) => unsafe { std::env::set_var(&k, v) }, None => unsafe { std::env::remove_var(&k) } } }
}

/// 12. A new layer becomes the whole selection, so a delete afterwards removes it, not the old selection.
#[test]
fn new_layers_replace_the_selection() {
    let mut d = Document::blank(4, 4, 72.0).unwrap();
    let a = d.active.unwrap();
    let b = d.add_blank_layer();
    d.select_layer(Some(a));
    d.toggle_layer_selected(b);
    assert_eq!(d.selected.len(), 2);
    let c = d.add_blank_layer();
    assert_eq!(d.selected, [c].into_iter().collect());
    d.delete_layer();
    assert!(d.has_layer(a) && d.has_layer(b) && !d.has_layer(c));
}
