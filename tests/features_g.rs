//! Photoshop commands added for parity: merge and stamp visible, layer via copy and cut, bring to front and
//! send to back, reselect, feather, and guide snapping with the grid.

use compositor::document::Document;
use compositor::selection::Mode;

fn alpha_at(d: &mut Document, x: usize, y: usize) -> u8 {
    let s = d.renderer.render_flat().unwrap();
    compositor::raster::with_bytes(&s, |b, stride| b[y * stride + x * 4 + 3]).unwrap()
}

#[test]
fn merge_and_stamp_visible_composite_the_visible_layers() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let a = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let b = d.add_shape_layer(false, (20.0, 20.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    let hidden = d.add_shape_layer(false, (30.0, 0.0, 10.0, 10.0), [0.0, 1.0, 0.0], 0.0).unwrap();
    d.set_visible(hidden, false);
    let before = d.renderer.layers().len();
    let stamp = d.stamp_visible().unwrap();
    assert_eq!(d.renderer.layers().len(), before + 1, "stamp adds a layer and keeps the rest");
    assert_eq!(d.renderer.image_size(stamp), Some((40, 40)));
    d.undo();
    let merged = d.merge_visible().unwrap();
    let names: Vec<String> = d.renderer.layers().iter().map(|l| l.name.clone()).collect();
    assert!(!d.has_layer(a) && !d.has_layer(b) && d.has_layer(hidden), "visible layers merged, the hidden one stays: {names:?}");
    assert_eq!(d.renderer.layer(merged).name, "Rectangle 2", "named after the topmost merged layer");
    assert_eq!(alpha_at(&mut d, 5, 5), 255);
    assert_eq!(alpha_at(&mut d, 25, 25), 255);
    assert_eq!(alpha_at(&mut d, 35, 5), 0, "the hidden layer is still hidden");
    assert_eq!(d.undo_name(), Some("Merge Visible"));
    d.undo();
    assert!(d.has_layer(a) && d.has_layer(b));
}

#[test]
fn layer_via_copy_and_cut_lift_the_selection_in_place() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let id = d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.select_box(10.0, 10.0, 10.0, 10.0, false, Mode::Replace, false).unwrap();
    let copy = d.layer_via(false).unwrap();
    let layer = d.renderer.layer(copy).clone();
    assert_eq!((layer.transform.origin.0, layer.transform.origin.1, layer.transform.size.0, layer.transform.size.1), (10.0, 10.0, 10.0, 10.0));
    assert_eq!(d.renderer.layers().len(), 3);
    assert_eq!(d.undo_name(), Some("Layer Via Copy"));
    d.undo();
    d.select_layer(Some(id));
    let cut = d.layer_via(true).unwrap();
    assert!(d.has_layer(cut));
    d.set_visible(cut, false);
    assert_eq!(alpha_at(&mut d, 15, 15), 0, "cut cleared the source");
    assert_eq!(alpha_at(&mut d, 5, 5), 255);
    d.undo();
    assert_eq!(alpha_at(&mut d, 15, 15), 255, "one undo puts the pixels back");
}

#[test]
fn front_back_reselect_and_feather() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let a = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let _b = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [0.0, 1.0, 0.0], 0.0).unwrap();
    let _c = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.select_layer(Some(a));
    d.move_layer_to_end(true);
    assert_eq!(d.renderer.layers().last().map(|l| l.id), Some(a), "brought to front");
    d.move_layer_to_end(false);
    assert_eq!(d.renderer.layers().first().map(|l| l.id), Some(a), "sent to back");
    d.select_all_layers();
    assert_eq!(d.selected.len(), 4);

    d.select_box(10.0, 10.0, 10.0, 10.0, false, Mode::Replace, false).unwrap();
    d.deselect();
    assert!(d.selection.is_none());
    d.reselect();
    assert_eq!(d.selection.as_ref().and_then(|s| s.bounds), Some((10, 10, 20, 20)));
    assert!(d.feather_selection(0.0).is_err());
    d.feather_selection(4.0).unwrap();
    let sel = d.selection.clone().unwrap();
    let at = |x: usize, y: usize| compositor::raster::with_bytes(&sel.mask, |b, stride| b[y * stride + x]).unwrap();
    assert!(at(15, 15) > 200, "center stays selected: {}", at(15, 15));
    assert!(at(10, 15) > 40 && at(10, 15) < 220, "the edge softened: {}", at(10, 15));
    assert!(at(7, 15) > 0, "feather spreads outward: {}", at(7, 15));
    assert_eq!(d.undo_name(), Some("Feather"));
}

#[test]
fn guides_snap_to_centers_layers_and_the_grid_unless_snap_is_off() {
    let mut d = Document::blank(200, 100, 72.0).unwrap();
    d.add_shape_layer(false, (30.0, 10.0, 20.0, 20.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    assert_eq!(d.snapped_guide(true, 97.0, 8.0), 100.0, "canvas center");
    assert_eq!(d.snapped_guide(true, 52.0, 8.0), 50.0, "layer's right edge");
    assert_eq!(d.snapped_guide(true, 41.0, 8.0), 40.0, "layer's center");
    assert_eq!(d.snapped_guide(false, 3.0, 8.0), 0.0, "canvas top");
    assert_eq!(d.snapped_guide(true, 70.0, 8.0), 70.0, "nothing near stays put");
    d.grid = Some((100.0, 4));
    let (gx, gy) = d.grid_lines();
    assert_eq!(gx, vec![0.0, 25.0, 50.0, 75.0, 100.0, 125.0, 150.0, 175.0, 200.0]);
    assert_eq!(gy.len(), 5);
    assert_eq!(d.snapped_guide(true, 73.0, 8.0), 75.0, "grid line");
    d.snap = false;
    assert_eq!(d.snapped_guide(true, 97.0, 8.0), 97.0, "snap off");
    let (xs, _) = d.snap_targets(&[]);
    assert!(xs.contains(&125.0), "moves snap to the grid too");
}

#[test]
fn free_transform_floats_the_selection_and_lands_it_as_one_step() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let id = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.select_box(0.0, 0.0, 10.0, 10.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    let (float, source) = d.floating.unwrap();
    assert_eq!(source, id);
    assert_eq!(d.renderer.layer(float).name, "Floating Selection");
    assert!(d.selection.is_none(), "the selection rides on the floating layer");
    let mut t = d.renderer.layer(float).transform;
    t.origin = compositor::format::Point(30.0, 30.0);
    d.set_transform(float, t, "Transform Layer");
    d.commit_free_transform().unwrap();
    assert!(d.floating.is_none());
    assert_eq!(d.renderer.layers().len(), 2, "merged back into the source");
    assert_eq!(alpha_at(&mut d, 5, 5), 0, "the pixels left their old place");
    assert_eq!(alpha_at(&mut d, 35, 35), 255, "and landed where the handles put them");
    assert_eq!(d.selection.as_ref().and_then(|s| s.bounds), Some((30, 30, 40, 40)), "the moved pixels stay selected");
    assert_eq!(d.undo_name(), Some("Free Transform"));
    d.undo();
    assert_eq!(alpha_at(&mut d, 5, 5), 255, "one undo puts it all back");
    // Escape cancels without a trace.
    d.select_box(0.0, 0.0, 10.0, 10.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    let float = d.floating.unwrap().0;
    let mut t = d.renderer.layer(float).transform;
    t.origin = compositor::format::Point(30.0, 30.0);
    d.set_transform(float, t, "Transform Layer");
    d.cancel_free_transform();
    assert!(d.floating.is_none() && d.renderer.layers().len() == 2);
    assert_eq!(alpha_at(&mut d, 5, 5), 255);
    assert_eq!(alpha_at(&mut d, 35, 35), 0);
    d.deselect();
    assert!(d.begin_free_transform().is_err(), "needs a selection");
}
