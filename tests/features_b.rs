//! Transform features: several layers as one box, moving and duplicating selected pixels, free distort.

mod common;

use common::*;
use compositor::distort;
use compositor::document::Document;
use compositor::selection::Mode;

fn flat(d: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; let a = p[3] as u32; let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 }; [un(p[2]), un(p[1]), un(p[0]), p[3]] }).collect()).unwrap();
    (px, w as u32)
}

#[test]
fn several_selected_layers_transform_as_one_box() {
    let mut f = Fixture::new("multi", 10, 10);
    let a = f.add(Spec { name: "A", origin: (0.0, 0.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, RED)), ..Default::default() });
    let b = f.add(Spec { name: "B", origin: (6.0, 6.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, BLUE)), ..Default::default() });
    let c = f.add(Spec { name: "C", origin: (4.0, 0.0), size: (2.0, 2.0), pixels: Some(solid(2, 2, WHITE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.select_layer(Some(a));
    assert!(!d.transforms_as_group());
    d.toggle_layer_selected(b);
    assert!(d.transforms_as_group() && d.selected.len() == 2 && d.active == Some(b));
    let mut members = d.transform_members(b);
    members.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(members, expected);
    let bx = d.group_box().unwrap();
    assert_eq!((bx.origin.0, bx.origin.1, bx.size.0, bx.size.1), (0.0, 0.0, 8.0, 8.0));
    d.nudge(1.0, 0.0);
    assert_eq!((d.renderer.layer(a).transform.origin.0, d.renderer.layer(b).transform.origin.0, d.renderer.layer(c).transform.origin.0), (1.0, 7.0, 4.0));
    d.flip_layer(true);
    // Mirrored about the box's middle (x = 5): A's left edge lands where B's right edge was.
    assert_eq!((d.renderer.layer(a).transform.origin.0, d.renderer.layer(b).transform.origin.0), (7.0, 1.0));
    // Shift-click extends from the active layer through the panel order.
    d.select_layer(Some(a));
    d.select_layer_range(c);
    assert_eq!(d.selected.len(), 3);
    assert_eq!(d.merge_title(), "Merge Layers");
    d.merge_layers().unwrap();
    assert_eq!(d.renderer.layers().len(), 1);
    assert_eq!(d.renderer.layers()[0].name, "C", "named after the topmost selected");
    d.undo();
    d.select_layer(Some(a));
    d.toggle_layer_selected(b);
    d.delete_layer();
    assert_eq!(d.renderer.layers().len(), 1);
    assert_eq!(d.renderer.layers()[0].id, c);
    assert_eq!(d.selected.len(), 1);
    // Toggling the only selected layer off keeps it.
    d.toggle_layer_selected(c);
    assert!(d.selected.contains(&c));
}

#[test]
fn selected_pixels_move_and_duplicate() {
    let mut f = Fixture::new("pixel-move", 8, 4);
    let id = f.add(Spec { size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, vec![255; 16])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.select_box(0.0, 0.0, 2.0, 4.0, false, Mode::Replace, false).unwrap();
    assert!(d.begin_pixel_move(false).unwrap());
    d.move_pixels(2.0, 0.0).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 0, 4, 4)), "the outline follows during the drag");
    d.finish_pixel_move().unwrap();
    assert_eq!(d.undo_name(), Some("Move Pixels"));
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 1, CLEAR, 0);
    assert_pixel(&px, w, 3, 1, RED, 0);
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 0, 4, 4)));
    d.undo();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 1, RED, 0);
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((0, 0, 2, 4)), "undo brings the outline back");
    // Past the layer's edge the layer grows, its mask revealing the new area.
    d.nudge_pixels(-1.0, 0.0).unwrap();
    let t = d.renderer.layer(id).transform;
    assert_eq!((t.origin.0, t.size.0), (-1.0, 5.0), "{t:?}");
    assert_eq!(d.renderer.mask(id).unwrap().width(), 5);
    d.undo();
    // Duplicating leaves the original in place.
    assert!(d.begin_pixel_move(true).unwrap());
    d.move_pixels(2.0, 0.0).unwrap();
    d.finish_pixel_move().unwrap();
    assert_eq!(d.undo_name(), Some("Duplicate Pixels"));
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 1, RED, 0);
    assert_pixel(&px, w, 3, 1, RED, 0);
    // Nothing to move without a selection, and a mask target never lifts pixels.
    d.deselect();
    assert!(!d.begin_pixel_move(false).unwrap());
    d.select_all().unwrap();
    d.set_mask_target(true);
    assert!(!d.begin_pixel_move(false).unwrap());
}

#[test]
fn free_distort_resamples_into_the_shape() {
    let mut f = Fixture::new("distort", 12, 12);
    let id = f.add(Spec { origin: (2.0, 2.0), size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, vec![255; 16])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    let original = d.renderer.layer(id).transform;
    let mut corners = distort::corners(&original);
    assert_eq!(corners, [(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)]);
    corners[2] = (10.0, 10.0);
    d.preview_distort(id, &original, &corners).unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 7, 7, RED, 2);
    assert_pixel(&px, w, 3, 3, RED, 2);
    d.commit_distort(&[(id, original, corners)]).unwrap();
    assert_eq!(d.undo_name(), Some("Distort"));
    let t = d.renderer.layer(id).transform;
    assert_eq!((t.origin.0, t.origin.1, t.size.0, t.size.1, t.rotation), (2.0, 2.0, 8.0, 8.0, 0.0), "an upright layer over the shape: {t:?}");
    assert_eq!(d.renderer.image(id).unwrap().width(), 8);
    assert_eq!(d.renderer.mask(id).unwrap().width(), 8, "the mask warped with it");
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 3, 3, RED, 2);
    assert_pixel(&px, w, 7, 7, RED, 2);
    assert_pixel(&px, w, 9, 3, CLEAR, 0, );
    d.undo();
    assert_eq!(d.renderer.layer(id).transform, original);
    // A twisted shape is refused and leaves the layer as it was.
    let twisted = [(2.0, 2.0), (2.0, 6.0), (6.0, 2.0), (6.0, 6.0)];
    assert!(d.commit_distort(&[(id, original, twisted)]).is_err());
    assert_eq!(d.renderer.layer(id).transform, original);
    assert!(d.can_redo(), "the refused edit left history alone");
}
