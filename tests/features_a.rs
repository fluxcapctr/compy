//! Layer and mask commands from the reference's feature list: merge, invert, flip canvas, fill and clear,
//! filters and the blur tool on masks, a mask as a selection, content-aware fill past the layer's edge.

mod common;

use common::*;
use compositor::brush::BrushSettings;
use compositor::document::Document;
use compositor::filters::{Kind, Settings};
use compositor::selection::Mode;

fn flat(d: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; let a = p[3] as u32; let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 }; [un(p[2]), un(p[1]), un(p[0]), p[3]] }).collect()).unwrap();
    (px, w as u32)
}
fn mask_bytes(d: &Document, id: uuid::Uuid) -> Vec<u8> {
    let m = d.renderer.mask(id).unwrap();
    let (w, h) = (m.width() as usize, m.height() as usize);
    compositor::raster::with_bytes(m, |b, stride| (0..h).flat_map(|y| b[y * stride..y * stride + w].to_vec()).collect()).unwrap()
}

#[test]
fn merge_down_bakes_the_pair_into_one_layer() {
    let mut f = Fixture::new("merge-down", 4, 4);
    let below = f.add(Spec { name: "Below", size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let above = f.add(Spec { name: "Above", origin: (1.0, 1.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, BLUE)), opacity: Some(0.5), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(above);
    let (before, _) = flat(&mut d);
    assert_eq!(d.merge_title(), "Merge Down");
    d.merge_layers().unwrap();
    assert_eq!(d.renderer.layers().len(), 1);
    let merged = &d.renderer.layers()[0];
    assert_eq!(merged.name, "Below");
    assert!(!d.has_layer(below) && !d.has_layer(above));
    assert_eq!(d.active, Some(merged.id));
    assert_eq!((merged.transform.origin.0, merged.transform.size.0), (0.0, 4.0));
    let (after, _) = flat(&mut d);
    for (a, b) in before.iter().zip(&after) { assert!(close(*a, *b, 1), "{a:?} vs {b:?}"); }
    assert_eq!(d.undo_name(), Some("Merge Down"));
    d.undo();
    assert_eq!(d.renderer.layers().len(), 2);
}

#[test]
fn merge_group_flattens_a_folder_and_trims() {
    let mut f = Fixture::new("merge-group", 8, 8);
    let folder = f.add(Spec { name: "Art", group: true, size: (8.0, 8.0), ..Default::default() });
    f.add(Spec { name: "A", origin: (2.0, 2.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, RED)), parent: Some(folder), ..Default::default() });
    f.add(Spec { name: "B", origin: (4.0, 4.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, BLUE)), parent: Some(folder), ..Default::default() });
    let top = f.add(Spec { name: "Top", size: (8.0, 8.0), pixels: Some(solid(8, 8, CLEAR)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(folder);
    assert_eq!(d.merge_title(), "Merge Group");
    d.merge_layers().unwrap();
    assert_eq!(d.renderer.layers().len(), 2);
    let merged = d.renderer.layers().iter().find(|l| l.name == "Art").unwrap();
    assert!(!merged.is_group() && merged.parent_id.is_none());
    assert_eq!((merged.transform.origin.0, merged.transform.origin.1, merged.transform.size.0, merged.transform.size.1), (2.0, 2.0, 4.0, 4.0), "trimmed to its pixels");
    assert!(d.has_layer(top));
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 2, 2, RED, 0);
    assert_pixel(&px, w, 5, 5, BLUE, 0);
    assert_pixel(&px, w, 0, 0, CLEAR, 0);
}

#[test]
fn merge_reclips_and_refuses_the_impossible() {
    let mut f = Fixture::new("merge-clip", 4, 4);
    let base = f.add(Spec { name: "Base", size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let mid = f.add(Spec { name: "Mid", size: (4.0, 4.0), pixels: Some(solid(4, 4, BLUE)), ..Default::default() });
    let clipped = f.add(Spec { name: "Clip", size: (4.0, 4.0), pixels: Some(solid(4, 4, WHITE)), mask_source: Some(mid), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(mid);
    d.merge_layers().unwrap();
    let merged = d.renderer.layers().iter().find(|l| l.name == "Base").unwrap().id;
    assert_ne!(merged, base);
    assert_eq!(d.renderer.layer(clipped).mask_source_id, Some(merged), "the clipped layer follows the result");
    // The bottom layer has nothing beneath it.
    d.active = Some(merged);
    assert!(!d.can_merge());
    d.merge_layers().unwrap();
    assert_eq!(d.renderer.layers().len(), 2);
}

#[test]
fn invert_keeps_transparency_and_respects_the_selection() {
    let mut f = Fixture::new("invert", 4, 2);
    let id = f.add(Spec { size: (4.0, 2.0), pixels: Some(halves(4, 2, 2, RED, CLEAR)), mask: Some((4, 2, vec![255, 255, 0, 0, 255, 255, 0, 0])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.invert().unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, [0, 255, 255, 255], 0);
    assert_pixel(&px, w, 3, 0, CLEAR, 0);
    assert_eq!(d.undo_name(), Some("Invert"));
    d.undo();
    d.select_box(0.0, 0.0, 1.0, 2.0, false, Mode::Replace, false).unwrap();
    d.invert().unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, [0, 255, 255, 255], 0);
    assert_pixel(&px, w, 1, 0, RED, 0, );
    // On the mask: black and white swap inside the selection.
    d.deselect();
    d.set_mask_target(true);
    d.invert().unwrap();
    assert_eq!(mask_bytes(&d, id), vec![0, 0, 255, 255, 0, 0, 255, 255]);
    assert_eq!(d.undo_name(), Some("Invert Mask"));
}

#[test]
fn flip_canvas_mirrors_layers_masks_and_selection() {
    let mut f = Fixture::new("flip-canvas", 8, 4);
    let id = f.add(Spec { origin: (0.0, 0.0), size: (2.0, 4.0), pixels: Some(solid(2, 4, RED)), rotation: 10.0, mask: Some((1, 1, vec![255])), mask_placement: Some(((1.0, 0.0), (2.0, 4.0))), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.select_box(0.0, 0.0, 2.0, 4.0, false, Mode::Replace, false).unwrap();
    d.flip_canvas(true).unwrap();
    let t = d.renderer.layer(id).transform;
    assert_eq!((t.origin.0, t.rotation, t.flip_x), (6.0, -10.0, true));
    assert_eq!(d.renderer.layer(id).mask_placement.unwrap().origin.0, 5.0);
    let bounds = d.selection.as_ref().unwrap().bounds.unwrap();
    assert_eq!((bounds.0, bounds.2), (6, 8), "the selection crossed over: {bounds:?}");
    d.flip_canvas(false).unwrap();
    assert_eq!(d.renderer.layer(id).transform.flip_y, true);
    d.undo(); d.undo();
    assert_eq!(d.renderer.layer(id).transform.origin.0, 0.0);
}

#[test]
fn fill_and_clear_work_on_pixels_and_masks() {
    let mut f = Fixture::new("fill", 4, 4);
    let id = f.add(Spec { size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, vec![255; 16])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.select_box(0.0, 0.0, 2.0, 4.0, false, Mode::Replace, false).unwrap();
    d.fill([0.0, 0.0, 1.0]).unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, BLUE, 0);
    assert_pixel(&px, w, 3, 0, RED, 0);
    d.clear_selection(false).unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, CLEAR, 0);
    assert_pixel(&px, w, 3, 0, RED, 0);
    assert_eq!(d.undo_name(), Some("Clear"));
    // On the mask: fill black hides the selection; Delete puts the background tone there.
    d.set_mask_target(true);
    d.fill([0.0; 3]).unwrap();
    assert_eq!(mask_bytes(&d, id)[0], 0);
    assert_eq!(mask_bytes(&d, id)[3], 255);
    assert_eq!(d.undo_name(), Some("Fill Mask"));
    d.clear_selection(true).unwrap();
    assert_eq!(mask_bytes(&d, id)[0], 255);
    // A blank layer takes a fill over its whole size without a selection.
    let mut blank = Document::blank(3, 3, 72.0).unwrap();
    blank.fill([0.0, 1.0, 0.0]).unwrap();
    let (px, w) = flat(&mut blank);
    assert_pixel(&px, w, 2, 2, [0, 255, 0, 255], 0);
}

#[test]
fn filters_run_on_masks_and_the_blur_tool_softens_them() {
    let mut f = Fixture::new("mask-filter", 8, 4);
    let id = f.add(Spec { size: (8.0, 4.0), pixels: Some(solid(8, 4, RED)), mask: Some((8, 4, (0..32).map(|i| if i % 8 < 4 { 0 } else { 255 }).collect())), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.set_mask_target(true);
    let mut settings = Settings::default();
    settings.radius = 1.5;
    d.preview_filter(Kind::GaussianBlur, &settings).unwrap();
    d.clear_preview();
    assert_eq!(mask_bytes(&d, id)[3], 0, "a preview commits nothing");
    d.apply_filter(Kind::GaussianBlur, &settings).unwrap();
    let m = mask_bytes(&d, id);
    assert!(m[3] > 0 && m[3] < 255 && m[4] > 0 && m[4] < 255, "feathered edge: {:?}", &m[..8]);
    assert_eq!(m[0], 0);
    assert_eq!(d.undo_name(), Some("Gaussian Blur Mask"));
    d.undo();
    // The Smear tool on a mask blurs it under the tip.
    d.begin_mask_blur((4.0, 2.0), &BrushSettings { diameter: 3.0, hardness: 1.0, opacity: 1.0, ..Default::default() }).unwrap();
    d.finish_stroke().unwrap();
    let m = mask_bytes(&d, id);
    assert!(m[2 * 8 + 3] > 0 || m[2 * 8 + 4] < 255, "softened under the tip: {:?}", &m[16..24]);
    assert_eq!(m[0], 0, "untouched away from the tip");
}

#[test]
fn a_mask_loads_as_a_selection() {
    let mut f = Fixture::new("mask-select", 8, 4);
    let id = f.add(Spec { origin: (2.0, 0.0), size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, (0..16).map(|i| if i % 4 < 2 { 0 } else { 255 }).collect())), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.select_mask_pixels(id, Mode::Replace).unwrap();
    let s = d.selection.as_ref().unwrap();
    assert_eq!(s.bounds, Some((4, 0, 6, 4)), "{:?}", s.bounds);
    assert!(s.contains(4.5, 1.5) && !s.contains(2.5, 1.5) && !s.contains(7.5, 1.5));
}

#[test]
fn content_aware_fill_extends_past_the_layer_edge() {
    let mut f = Fixture::new("caf-extend", 8, 4);
    let id = f.add(Spec { size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.select_box(3.0, 0.0, 3.0, 4.0, false, Mode::Replace, false).unwrap();
    d.apply_filter(Kind::ContentAwareFill, &Settings::default()).unwrap();
    let t = d.renderer.layer(id).transform;
    assert_eq!((t.origin.0, t.size.0), (0.0, 6.0), "the layer grew to the selection: {t:?}");
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 5, 2, RED, 8, );
    assert_pixel(&px, w, 6, 2, CLEAR, 0);
}
