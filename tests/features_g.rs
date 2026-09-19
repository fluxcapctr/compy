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
    assert!(d.renderer.layer(cut).is_visible, "the first undo only shows the cut layer again");
    assert_eq!(d.undo_name(), Some("Layer Via Cut"));
    d.undo();
    assert!(!d.has_layer(cut), "the cut itself undone: the copy is gone");
    d.set_visible(id, true);
    assert_eq!(alpha_at(&mut d, 15, 15), 255, "and the source has its pixels back");
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
    let grown = d.renderer.layer(id).transform;
    assert_eq!((grown.origin.0, grown.origin.1, grown.size.0, grown.size.1), (0.0, 0.0, 40.0, 40.0), "the source grid grew to hold the landing");
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


#[test]
fn free_transform_keeps_off_canvas_pixels_merges_into_its_source_and_selects_only_the_moved_part() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    // A 20 by 20 layer half off the left edge, and an unrelated layer above it.
    let id = d.add_shape_layer(false, (0.0, 0.0, 20.0, 20.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let mut t = d.renderer.layer(id).transform;
    t.origin = compositor::format::Point(-10.0, 0.0);
    d.set_transform(id, t, "Move");
    let other = d.add_shape_layer(false, (30.0, 30.0, 5.0, 5.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.select_layer(Some(id));
    d.select_box(2.0, 2.0, 2.0, 2.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    // Reorder the float above the other layer: the commit must still land on its source.
    d.move_layer_to_end(true);
    let float = d.floating.unwrap().0;
    let mut ft = d.renderer.layer(float).transform;
    ft.origin = compositor::format::Point(20.0, 20.0);
    d.set_transform(float, ft, "Move");
    d.commit_free_transform().unwrap();
    assert!(d.has_layer(other) && d.has_layer(id) && d.renderer.layers().len() == 3);
    let t = d.renderer.layer(id).transform;
    assert_eq!(t.origin.0, -10.0, "nothing off the canvas was lost");
    assert!(t.size.0 >= 32.0, "the grid grew to hold the landing: {:?}", t.size);
    assert_eq!(alpha_at(&mut d, 21, 21), 255);
    assert_eq!(alpha_at(&mut d, 2, 2), 0, "cut from its old place");
    assert_eq!(alpha_at(&mut d, 5, 5), 255, "the rest of the layer stayed");
    assert_eq!(d.selection.as_ref().and_then(|s| s.bounds), Some((20, 20, 22, 22)), "only the moved pixels are selected");
}

#[test]
fn free_transform_counts_as_modified_and_lands_before_saving() {
    let mut d = Document::blank(20, 20, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let path = std::env::temp_dir().join(format!("compositor-test-{}-float.comp", std::process::id()));
    d.save(&path).unwrap();
    assert!(!d.is_modified());
    d.select_box(0.0, 0.0, 5.0, 5.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    assert!(d.is_modified(), "a float in progress is unsaved work");
    let float = d.floating.unwrap().0;
    let mut t = d.renderer.layer(float).transform;
    t.origin = compositor::format::Point(10.0, 10.0);
    d.set_transform(float, t, "Move");
    d.save(&path).unwrap();
    assert!(d.floating.is_none(), "saving lands the float first");
    assert!(!d.is_modified());
    let reopened = Document::new(compositor::format::load(&path).unwrap()).unwrap();
    assert_eq!(reopened.renderer.layers().len(), 2, "no floating layer in the file");
}

#[test]
fn merge_visible_keeps_hidden_children_of_merged_folders() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let folder = d.add_folder();
    d.select_layer(Some(folder));
    let shown = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.select_layer(Some(folder));
    let hidden = d.add_shape_layer(false, (20.0, 20.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.set_visible(hidden, false);
    assert_eq!(d.renderer.layer(hidden).parent_id, Some(folder));
    d.merge_visible().unwrap();
    assert!(!d.has_layer(folder) && !d.has_layer(shown));
    assert!(d.has_layer(hidden), "the hidden child survives");
    assert_eq!(d.renderer.layer(hidden).parent_id, None, "moved up out of the merged folder");
    assert_eq!(alpha_at(&mut d, 5, 5), 255);
}

fn rgb_at(d: &mut Document, x: usize, y: usize) -> [u8; 4] {
    let s = d.renderer.render_flat().unwrap();
    compositor::raster::with_bytes(&s, |b, stride| { let p = &b[y * stride + x * 4..][..4]; [p[2], p[1], p[0], p[3]] }).unwrap()
}

#[test]
fn curves_color_balance_auto_levels_and_fade() {
    use compositor::filters::{Kind, Settings};
    let mut d = Document::blank(20, 20, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 20.0, 20.0), [0.5, 0.5, 0.5], 0.0).unwrap();
    assert_eq!(rgb_at(&mut d, 5, 5)[0], 128);
    // A curve lifting the midpoint brightens a mid gray; the identity curve is a no-op that leaves no step.
    let mut s = Settings::default();
    d.apply_filter(Kind::Curves, &s).unwrap();
    assert_ne!(d.undo_name(), Some("Curves"));
    s.curves.channels[0] = vec![(0.0, 0.0), (128.0, 192.0), (255.0, 255.0)];
    d.apply_filter(Kind::Curves, &s).unwrap();
    assert_eq!(d.undo_name(), Some("Curves"));
    let lifted = rgb_at(&mut d, 5, 5);
    assert!(lifted[0] > 180 && lifted[1] == lifted[0] && lifted[2] == lifted[0], "{lifted:?}");
    // Fade halves the change; a second fade is refused because the layer moved on.
    let mut f = Settings::default();
    f.fade = 0.5;
    d.apply_filter(Kind::Fade, &f).unwrap();
    let faded = rgb_at(&mut d, 5, 5)[0] as i32;
    assert!((faded - (128 + lifted[0] as i32) / 2).abs() <= 2, "faded {faded} between 128 and {}", lifted[0]);
    assert_eq!(d.undo_name(), Some("Fade"));
    assert!(d.apply_filter(Kind::Fade, &f).is_err(), "nothing to fade twice");
    d.undo(); d.undo();
    assert_eq!(rgb_at(&mut d, 5, 5)[0], 128);
    // Color Balance toward red in the midtones, preserving luminosity.
    let mut b = Settings::default();
    b.balance.midtones = [100.0, 0.0, 0.0];
    d.apply_filter(Kind::ColorBalance, &b).unwrap();
    let warm = rgb_at(&mut d, 5, 5);
    assert!(warm[0] > warm[1] && warm[0] > warm[2], "red shifted: {warm:?}");
    let luma = |c: [u8; 4]| 0.299 * c[0] as f64 + 0.587 * c[1] as f64 + 0.114 * c[2] as f64;
    assert!((luma(warm) - 128.0).abs() < 4.0, "luminosity kept: {warm:?}");
    d.undo();
    // Auto Contrast on a low-contrast gradient spreads it to full range; Auto Tone works per channel.
    let mut low = Document::blank(64, 1, 72.0).unwrap();
    let id = low.add_shape_layer(false, (0.0, 0.0, 64.0, 1.0), [0.0, 0.0, 0.0], 0.0).unwrap();
    let image = low.renderer.image(id).unwrap().clone();
    compositor::raster::with_bytes_raw_mut(&image, |data, _| { for x in 0..64 { let v = 80 + x as u8; data[x * 4] = v; data[x * 4 + 1] = v; data[x * 4 + 2] = v; data[x * 4 + 3] = 255; } }).unwrap();
    low.renderer.set_image(id, image);
    low.auto_levels(compositor::document::AutoLevels::Contrast).unwrap();
    let (dark, bright) = (rgb_at(&mut low, 0, 0)[0], rgb_at(&mut low, 63, 0)[0]);
    assert!(dark < 10 && bright > 245, "stretched to the ends: {dark} {bright}");
    assert_eq!(low.undo_name(), Some("Levels"));
    low.undo();
    low.auto_levels(compositor::document::AutoLevels::Tone).unwrap();
    assert!(rgb_at(&mut low, 63, 0)[0] > 245);
    low.undo();
    low.auto_levels(compositor::document::AutoLevels::Color).unwrap();
    let mid = rgb_at(&mut low, 32, 0)[0] as i32;
    assert!((mid - 128).abs() < 20, "midtone pulled to gray: {mid}");
}
