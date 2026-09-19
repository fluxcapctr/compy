mod common;
use common::*;
use compositor::brush::BrushSettings;
use compositor::document::{Document, StrokeKind};
use compositor::format::{Point, Size, Transform};
use compositor::raster::with_bytes;
use compositor::selection::Mode;
use compositor::transform::{Drag, Mode as DragMode, snap_offset};

fn doc(f: &Fixture) -> Document { Document::new(f.load().unwrap()).unwrap() }
fn flat(d: &mut Document) -> Vec<[u8; 4]> {
    let image = d.renderer.render_flat().unwrap();
    let (w, h) = (image.width() as usize, image.height() as usize);
    with_bytes(&image, |data, stride| (0..w * h).map(|i| {
        let p = &data[(i / w) * stride + (i % w) * 4..][..4];
        let a = p[3] as u32;
        let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
        [un(p[2]), un(p[1]), un(p[0]), p[3]]
    }).collect()).unwrap()
}
fn t(x: f64, y: f64, w: f64, h: f64) -> Transform { Transform { origin: Point(x, y), size: Size(w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() } }

#[test]
fn placements_follow_moves_and_scales() {
    let old = t(0.0, 0.0, 10.0, 10.0);
    let moved = t(5.0, 3.0, 10.0, 10.0);
    let mask = t(2.0, 2.0, 4.0, 4.0);
    let carried = mask.following(&old, &moved);
    assert_eq!((carried.origin.0, carried.origin.1), (7.0, 5.0));
    let scaled = t(0.0, 0.0, 20.0, 20.0);
    let carried = mask.following(&old, &scaled);
    assert!((carried.origin.0 - 4.0).abs() < 1e-6 && (carried.size.0 - 8.0).abs() < 1e-6, "{carried:?}");
    let flipped = t(0.0, 0.0, 10.0, 10.0).mirrored(true, 20.0);
    assert!(flipped.flip_x && (flipped.origin.0 - 30.0).abs() < 1e-9);
    let mut rotated = t(0.0, 0.0, 10.0, 10.0);
    rotated.rotation = 30.0;
    assert_eq!(rotated.mirrored(false, 0.0).rotation, -30.0);
    assert!(rotated.contains((5.0, 5.0)) && !rotated.contains((-3.0, -3.0)));
}

#[test]
fn drags_resize_rotate_and_snap() {
    let original = t(0.0, 0.0, 10.0, 10.0);
    let drag = Drag { original, start: (10.0, 10.0), mode: DragMode::Resize(4) };
    let bigger = drag.updated((20.0, 20.0), false, false, false);
    assert!((bigger.size.0 - 20.0).abs() < 1e-9 && (bigger.size.1 - 20.0).abs() < 1e-9 && bigger.origin.0.abs() < 1e-9, "{bigger:?}");
    let centered = drag.updated((20.0, 20.0), false, false, true);
    assert!((centered.size.0 - 30.0).abs() < 1e-9 && (centered.origin.0 + 10.0).abs() < 1e-9, "{centered:?}");
    let proportional = Drag { original, start: (10.0, 5.0), mode: DragMode::Resize(3) }.updated((20.0, 5.0), true, false, false);
    assert!((proportional.size.1 - 20.0).abs() < 1e-9, "{proportional:?}");
    let rotate = Drag { original, start: (10.0, 5.0), mode: DragMode::Rotate };
    let turned = rotate.updated((5.0, 10.0), false, true, false);
    assert_eq!(turned.rotation, 90.0);
    let moved = Drag { original, start: (5.0, 5.0), mode: DragMode::Move }.updated((12.0, 6.0), false, true, false);
    assert_eq!((moved.origin.0, moved.origin.1), (7.0, 0.0), "shift keeps one axis");
    let ((dx, dy), gx, gy) = snap_offset((97.0, 40.0, 117.0, 60.0), &[0.0, 100.0], &[0.0, 50.0], 5.0);
    assert_eq!((dx, dy), (3.0, 0.0));
    assert_eq!((gx, gy), (Some(100.0), Some(50.0)));
}

#[test]
fn moving_a_layer_carries_a_linked_mask_and_leaves_an_unlinked_one() {
    let mut f = Fixture::new("carry", 12, 12);
    let id = f.add(Spec { name: "Red", origin: (2.0, 2.0), size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, vec![255; 16])), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.nudge(3.0, 0.0);
    let layer = d.renderer.layer(id).clone();
    assert_eq!(layer.transform.origin.0, 5.0);
    assert!(layer.mask_placement.is_none(), "a linked mask keeps covering the layer");
    d.toggle_mask_linked(id);
    d.nudge(0.0, 2.0);
    let layer = d.renderer.layer(id).clone();
    assert_eq!(layer.mask_placement.map(|p| (p.origin.0, p.origin.1)), Some((5.0, 2.0)), "an unlinked mask stays put");
    d.undo(); d.undo(); d.undo();
    assert_eq!(d.renderer.layer(id).transform.origin.0, 2.0);
    d.flip_layer(true);
    assert!(d.renderer.layer(id).transform.flip_x);
    assert_eq!(d.undo_name(), Some("Flip Horizontal"));
}

#[test]
fn marquee_lasso_and_outline_moves() {
    let f = Fixture::new("shapes", 10, 10);
    let mut d = doc(&f);
    d.select_box(2.0, 2.0, 4.0, 4.0, false, Mode::Replace, false).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 2, 6, 6)));
    d.select_box(0.0, 0.0, 10.0, 10.0, true, Mode::Replace, false).unwrap();
    let sel = d.selection.clone().unwrap();
    assert!(sel.contains(5.0, 5.0) && !sel.contains(0.0, 0.0), "an ellipse fills the middle, not the corner");
    d.select_polygon(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)], Mode::Replace, false, "Lasso").unwrap();
    let sel = d.selection.clone().unwrap();
    assert!(sel.contains(2.0, 2.0) && !sel.contains(8.0, 8.0));
    d.select_box(5.0, 5.0, 5.0, 5.0, false, Mode::Add, false).unwrap();
    assert!(d.selection.as_ref().unwrap().contains(8.0, 8.0));
    d.select_box(0.0, 0.0, 2.0, 2.0, false, Mode::Replace, false).unwrap();
    d.move_selection(3.0, 0.0).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((3, 0, 5, 2)));
    assert_eq!(d.undo_name(), Some("Move Selection"));
    d.select_polygon(&[(1.0, 1.0)], Mode::Replace, false, "Lasso").unwrap();
    assert!(d.selection.is_none(), "a click enclosing nothing deselects");
}

#[test]
fn masks_are_added_toggled_inverted_and_deleted() {
    let mut f = Fixture::new("masks", 4, 1);
    let id = f.add(Spec { name: "Red", size: (4.0, 1.0), pixels: Some(solid(4, 1, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.add_mask(false).unwrap();
    assert!(d.renderer.mask(id).is_some() && d.renderer.layer(id).mask_file.is_some() && d.mask_target());
    assert!(flat(&mut d).iter().all(|p| *p == CLEAR), "hide-all hides");
    d.toggle_mask_enabled();
    assert_eq!(flat(&mut d)[0], RED);
    d.toggle_mask_enabled();
    d.invert_mask().unwrap();
    assert_eq!(flat(&mut d)[0], RED, "inverted hide-all reveals");
    d.delete_mask();
    assert!(d.renderer.mask(id).is_none() && d.renderer.layer(id).mask_file.is_none());
    d.select_box(0.0, 0.0, 2.0, 1.0, false, Mode::Replace, false).unwrap();
    d.add_mask(true).unwrap();
    let px = flat(&mut d);
    assert_eq!((px[0], px[3]), (CLEAR, RED), "a white mask hides the selection");
    assert!(d.selection.is_none(), "the selection is used up");
    assert_eq!(d.undo_name(), Some("Add Mask from Selection"));
    d.undo();
    assert!(d.renderer.mask(id).is_none() && d.selection.is_some());
}

#[test]
fn painting_a_mask_hides_pixels_under_the_stroke() {
    let mut f = Fixture::new("mask-paint", 8, 8);
    let id = f.add(Spec { name: "Red", size: (8.0, 8.0), pixels: Some(solid(8, 8, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.add_mask(true).unwrap();
    let settings = BrushSettings { diameter: 4.0, hardness: 1.0, color: [0.0; 3], opacity: 1.0, ..Default::default() };
    d.begin_mask_stroke((4.0, 4.0), &settings, false).unwrap();
    let during = flat(&mut d);
    assert_eq!(during[4 * 8 + 4], CLEAR, "the live preview hides under the tip");
    d.finish_stroke().unwrap();
    let px = flat(&mut d);
    assert_eq!(px[4 * 8 + 4], CLEAR);
    assert_eq!(px[0], RED);
    assert_eq!(d.renderer.mask(id).map(|m| (m.width(), m.height())), Some((8, 8)), "the 1 x 1 mask grew to the layer's grid");
    assert_eq!(d.undo_name(), Some("Paint Mask"));
    d.begin_mask_stroke((4.0, 4.0), &settings, true).unwrap();
    d.finish_stroke().unwrap();
    assert_eq!(flat(&mut d)[4 * 8 + 4], RED, "white reveals again");
    d.undo(); d.undo();
    assert_eq!(flat(&mut d)[4 * 8 + 4], RED);
    // Ordinary strokes still go to the pixels when the mask is not the target.
    d.begin_stroke((4.0, 4.0), &settings, StrokeKind::Erase).unwrap();
    d.finish_stroke().unwrap();
    assert_eq!(flat(&mut d)[4 * 8 + 4], CLEAR);
}

#[test]
fn clipping_toggles_follow_the_stack_rules() {
    let mut f = Fixture::new("clip-toggle", 2, 1);
    let base = f.add(Spec { name: "Base", size: (2.0, 1.0), pixels: Some(halves(2, 1, 1, RED, CLEAR)), ..Spec::default() });
    let a = f.add(Spec { name: "A", size: (2.0, 1.0), pixels: Some(solid(2, 1, BLUE)), ..Spec::default() });
    let b = f.add(Spec { name: "B", size: (2.0, 1.0), pixels: Some(solid(2, 1, WHITE)), ..Spec::default() });
    let folder = f.add(Spec { name: "Folder", group: true, ..Spec::default() });
    let mut d = doc(&f);
    d.toggle_clipping(a);
    assert_eq!(d.renderer.layer(a).mask_source_id, Some(base));
    assert_eq!(flat(&mut d)[1], WHITE, "B is unclipped");
    d.toggle_clipping(b);
    assert_eq!(d.renderer.layer(b).mask_source_id, Some(base), "B shares A's base");
    assert_eq!(flat(&mut d)[1], CLEAR);
    d.toggle_clipping(a);
    assert!(d.renderer.layer(a).mask_source_id.is_none() && d.renderer.layer(b).mask_source_id.is_none(), "releasing A releases B above it");
    d.toggle_clipping(folder);
    assert!(d.renderer.layer(folder).mask_source_id.is_none());
    assert!(!d.can_clip(folder, a) && !d.can_clip(a, a));
    d.toggle_clipping(base);
    assert!(d.renderer.layer(base).mask_source_id.is_none(), "nothing below the bottom layer");
}

#[test]
fn layer_pixels_load_as_a_selection() {
    let mut f = Fixture::new("load", 4, 1);
    let id = f.add(Spec { name: "Pair", size: (4.0, 1.0), pixels: Some(halves(4, 1, 2, RED, CLEAR)), ..Spec::default() });
    let mut d = doc(&f);
    d.select_layer_pixels(id, Mode::Replace).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((0, 0, 2, 1)));
}
