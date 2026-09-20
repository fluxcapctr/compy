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


#[test]
fn free_transform_refuses_an_oversized_merge_and_grows_a_mask_white() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let id = d.add_shape_layer(false, (0.0, 0.0, 20.0, 20.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.select_box(2.0, 2.0, 4.0, 4.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    let float = d.floating.unwrap().0;
    let mut ft = d.renderer.layer(float).transform;
    ft.size = compositor::format::Size(40_000.0, 40_000.0);
    d.set_transform(float, ft, "Scale");
    assert!(d.commit_free_transform().is_err(), "too large to land");
    assert!(d.floating.is_none() && d.renderer.layers().len() == 2, "everything back as before Ctrl+T");
    assert_eq!(alpha_at(&mut d, 3, 3), 255);
    assert_eq!(d.renderer.layer(id).transform.size.0, 20.0);
    assert_ne!(d.undo_name(), Some("Free Transform"));

    // A black-edged mask on the source grows white where the moved pixels land.
    let mask = compositor::raster::a8_filled(20, 20, 0).unwrap();
    compositor::raster::with_bytes_raw_mut(&mask, |b, stride| { for y in 4..16 { for x in 4..16 { b[y * stride + x] = 255; } } }).unwrap();
    d.renderer.set_mask(id, Some(mask));
    d.select_box(6.0, 6.0, 4.0, 4.0, false, Mode::Replace, false).unwrap();
    d.begin_free_transform().unwrap();
    let float = d.floating.unwrap().0;
    let mut ft = d.renderer.layer(float).transform;
    ft.origin = compositor::format::Point(30.0, 30.0);
    d.set_transform(float, ft, "Move");
    d.commit_free_transform().unwrap();
    assert_eq!(alpha_at(&mut d, 32, 32), 255, "the landed pixels show through the grown mask");
    assert_eq!(alpha_at(&mut d, 1, 1), 0, "the old black edge still hides the old corner");
}

#[test]
fn fade_is_only_offered_right_after_the_filter_and_auto_levels_refuse_a_mask() {
    use compositor::filters::{Kind, Settings};
    let mut d = Document::blank(20, 20, 72.0).unwrap();
    let id = d.add_shape_layer(false, (0.0, 0.0, 20.0, 20.0), [0.5, 0.5, 0.5], 0.0).unwrap();
    let mut s = Settings::default();
    s.curves.channels[0] = vec![(0.0, 0.0), (128.0, 192.0), (255.0, 255.0)];
    d.apply_filter(Kind::Curves, &s).unwrap();
    let mut t = d.renderer.layer(id).transform;
    t.origin = compositor::format::Point(1.0, 0.0);
    d.set_transform(id, t, "Move");
    let mut f = Settings::default();
    f.fade = 0.5;
    assert!(d.apply_filter(Kind::Fade, &f).is_err(), "a move in between ends the offer");
    d.apply_filter(Kind::Curves, &s).unwrap();
    d.undo();
    assert!(d.apply_filter(Kind::Fade, &f).is_err(), "undo ends it too");
    d.apply_filter(Kind::Curves, &s).unwrap();
    assert!(d.apply_filter(Kind::Fade, &f).is_ok(), "straight after the filter it works");
    let mask = compositor::raster::a8_filled(20, 20, 128).unwrap();
    d.renderer.set_mask(id, Some(mask));
    d.set_mask_target(true);
    assert!(d.auto_levels(compositor::document::AutoLevels::Tone).is_err(), "auto levels read pixels, not masks");
    assert!(d.histogram().is_err());
}

#[test]
fn color_balance_keeps_luminosity_even_when_a_channel_clips() {
    use compositor::filters::{Kind, Settings};
    let mut d = Document::blank(4, 4, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 4.0, 4.0), [0.0, 0.0, 0.0], 0.0).unwrap();
    let mut b = Settings::default();
    b.balance.shadows = [100.0, 0.0, 0.0];
    d.apply_filter(Kind::ColorBalance, &b).unwrap();
    let p = rgb_at(&mut d, 1, 1);
    let luma = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
    assert!(luma < 2.0, "black stays black in luminosity: {p:?}");
}

#[test]
fn pen_paths_fill_stroke_and_select() {
    use compositor::path::{Anchor, Path};
    let mut d = Document::blank(60, 60, 72.0).unwrap();
    let id = d.add_shape_layer(false, (0.0, 0.0, 60.0, 60.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    let mut path = Path::default();
    path.anchors.push(Anchor::corner((10.0, 10.0)));
    path.anchors.push(Anchor { point: (50.0, 10.0), handle_in: Some((30.0, 30.0)), handle_out: None });
    path.anchors.push(Anchor::corner((50.0, 50.0)));
    path.anchors.push(Anchor::corner((10.0, 50.0)));
    path.closed = true;
    // Fill in red on the white layer.
    d.fill_path(&path, [1.0, 0.0, 0.0]).unwrap();
    assert_eq!(rgb_at(&mut d, 30, 40), [255, 0, 0, 255]);
    assert_eq!(rgb_at(&mut d, 5, 5), [255, 255, 255, 255], "outside untouched");
    assert_eq!(d.undo_name(), Some("Fill Path"));
    d.undo();
    // Stroke along it with a small black brush: paint lands on the path, not inside it.
    let mut brush = compositor::brush::BrushSettings::default();
    brush.diameter = 4.0;
    brush.color = [0.0, 0.0, 0.0];
    d.stroke_path(&path, &brush).unwrap();
    assert!(rgb_at(&mut d, 30, 50)[0] < 60, "the bottom edge is painted: {:?}", rgb_at(&mut d, 30, 50));
    assert_eq!(rgb_at(&mut d, 30, 40)[0], 255, "the inside is not");
    d.undo();
    // A selection from the path, and an open path closes itself for it.
    d.select_path(&path, Mode::Replace).unwrap();
    let sel = d.selection.clone().unwrap();
    assert!(sel.contains(30.0, 40.0) && !sel.contains(5.0, 5.0));
    assert_eq!(d.undo_name(), Some("Path Selection"));
    let mut open = path.clone();
    open.closed = false;
    d.select_path(&open, Mode::Replace).unwrap();
    assert!(d.selection.as_ref().unwrap().contains(30.0, 40.0));
    let two = Path { anchors: path.anchors[..2].to_vec(), closed: false };
    assert!(d.select_path(&two, Mode::Replace).is_err());
    let _ = id;
}

#[test]
fn pen_fill_and_stroke_target_the_mask_when_it_is_the_target() {
    use compositor::path::{Anchor, Path};
    let mut d = Document::blank(60, 60, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 60.0, 60.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.add_mask(true).unwrap();
    d.set_mask_target(true);
    let mut path = Path::default();
    path.anchors.push(Anchor::corner((10.0, 10.0)));
    path.anchors.push(Anchor::corner((50.0, 10.0)));
    path.anchors.push(Anchor::corner((50.0, 50.0)));
    path.anchors.push(Anchor::corner((10.0, 50.0)));
    path.closed = true;
    // Filling black hides the layer inside the path: the composite there goes transparent, the pixels stay red.
    d.fill_path(&path, [0.0, 0.0, 0.0]).unwrap();
    assert_eq!(alpha_at(&mut d, 30, 30), 0, "hidden by the mask inside the path");
    assert_eq!(alpha_at(&mut d, 5, 5), 255, "shown outside it");
    assert_eq!(d.undo_name(), Some("Fill Path"));
    assert_eq!(rgb_at(&mut d, 30, 30)[3], 0);
    d.undo();
    assert_eq!(alpha_at(&mut d, 30, 30), 255, "undone as one step");
    // Stroking with a black brush along the path hides a band under the path, not the inside.
    d.set_mask_target(true);
    let mut brush = compositor::brush::BrushSettings::default();
    brush.diameter = 6.0;
    brush.color = [0.0, 0.0, 0.0];
    d.stroke_path(&path, &brush).unwrap();
    assert!(alpha_at(&mut d, 30, 50) < 40, "the edge is hidden: {}", alpha_at(&mut d, 30, 50));
    assert_eq!(alpha_at(&mut d, 30, 30), 255, "the inside is not");
    assert_eq!(d.undo_name(), Some("Paint Mask"));
}

#[test]
fn image_and_canvas_size_stop_at_a_hundred_megapixels() {
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    assert!(d.image_size(20_000, 20_000, 72.0, compositor::format::Sampling::High).is_err());
    assert!(d.canvas_size(20_000, 20_000, 4, None, None, "Canvas Size").is_err());
    assert_eq!(d.width(), 40, "nothing changed");
    assert!(d.image_size(9_000, 9_000, 72.0, compositor::format::Sampling::High).is_ok());
}

#[test]
fn sharpening_raises_edge_contrast_and_leaves_flat_color_alone() {
    use compositor::filters::{Kind, Settings};
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [0.5, 0.5, 0.5], 0.0).unwrap();
    d.add_shape_layer(false, (20.0, 0.0, 20.0, 40.0), [0.8, 0.8, 0.8], 0.0).unwrap();
    let id = d.stamp_visible().unwrap();
    d.select_layer(Some(id));
    let before_dark = rgb_at(&mut d, 19, 20)[0];
    let before_light = rgb_at(&mut d, 20, 20)[0];
    let mut s = Settings::default();
    s.sharpen.amount = 200.0;
    s.sharpen.radius = 1.5;
    d.apply_filter(Kind::UnsharpMask, &s).unwrap();
    assert!(rgb_at(&mut d, 19, 20)[0] < before_dark, "the dark side of the edge gets darker");
    assert!(rgb_at(&mut d, 20, 20)[0] > before_light, "the light side gets lighter");
    assert_eq!(rgb_at(&mut d, 5, 20)[0], before_dark, "flat color far from the edge is untouched");
    assert_eq!(d.undo_name(), Some("Unsharp Mask"));
    d.undo();
    d.apply_filter(Kind::SmartSharpen, &s).unwrap();
    assert!(rgb_at(&mut d, 20, 20)[0] > before_light);
    let p = rgb_at(&mut d, 20, 20);
    assert!(p[0] == p[1] && p[1] == p[2], "gray stays gray when only brightness is sharpened: {p:?}");
}

#[test]
fn simple_adjustments_behave_as_filters_and_as_layers() {
    use compositor::filters::{Adjustment, BlackWhite, BrightnessContrast, Kind, Posterize, Settings, Threshold};
    // Black & White: pure red comes out at Photoshop's 40 percent, white stays white.
    let bw = BlackWhite::default();
    assert!((bw.gray([1.0, 0.0, 0.0]) - 0.4).abs() < 1e-9);
    assert!((bw.gray([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-9);
    assert!((bw.gray([0.0, 0.0, 1.0]) - 0.2).abs() < 1e-9);
    // Brightness/Contrast: a monotonic table that keeps black and white in place at brightness alone.
    let t = BrightnessContrast { brightness: 60.0, contrast: 0.0 }.table();
    assert!(t.windows(2).all(|w| w[0] <= w[1]) && t[0] == 0.0 && (t[255] - 1.0).abs() < 1e-6 && t[128] > 0.5);
    let c = BrightnessContrast { brightness: 0.0, contrast: 50.0 }.table();
    assert!(c[64] < t[64] && c[192] > 0.75, "contrast pushes tones apart");
    // Posterize to two levels leaves only black and white.
    let p = Posterize { levels: 2.0 }.table();
    assert!(p.iter().all(|v| *v == 0.0 || *v == 1.0));
    // Threshold on a document, both ways.
    let mut d = Document::blank(20, 20, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 10.0, 20.0), [0.3, 0.3, 0.3], 0.0).unwrap();
    d.add_shape_layer(false, (10.0, 0.0, 10.0, 20.0), [0.7, 0.7, 0.7], 0.0).unwrap();
    let id = d.stamp_visible().unwrap();
    d.select_layer(Some(id));
    let mut s = Settings::default();
    s.threshold = Threshold { level: 128.0 };
    d.apply_filter(Kind::Threshold, &s).unwrap();
    assert_eq!(rgb_at(&mut d, 5, 5), [0, 0, 0, 255]);
    assert_eq!(rgb_at(&mut d, 15, 5), [255, 255, 255, 255]);
    d.undo();
    let layer = d.add_adjustment("Threshold").unwrap();
    assert_eq!(rgb_at(&mut d, 5, 5), [0, 0, 0, 255], "the adjustment layer thresholds what is below");
    let record = d.adjustment(layer).unwrap();
    assert!(matches!(record, Adjustment::Threshold(_)));
    // The record survives a round trip through the file format.
    let back = Adjustment::from_record(&record.to_record()).unwrap();
    assert_eq!(back, record);
    let vib = Adjustment::from_record(&Adjustment::Vibrance(compositor::filters::Vibrance { vibrance: 30.0, saturation: -10.0 }).to_record()).unwrap();
    assert_eq!(vib, Adjustment::Vibrance(compositor::filters::Vibrance { vibrance: 30.0, saturation: -10.0 }));
}

#[test]
fn stroke_color_range_align_and_distribute() {
    let mut d = Document::blank(60, 60, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 60.0, 60.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    d.select_box(20.0, 20.0, 20.0, 20.0, false, Mode::Replace, false).unwrap();
    // A centered 4 px stroke straddles the selection edge and leaves the middle alone.
    d.stroke_selection(4.0, [1.0, 0.0, 0.0], 1, 1.0).unwrap();
    assert_eq!(rgb_at(&mut d, 21, 30), [255, 0, 0, 255], "just inside the edge");
    assert_eq!(rgb_at(&mut d, 18, 30), [255, 0, 0, 255], "just outside the edge");
    assert_eq!(rgb_at(&mut d, 30, 30), [255, 255, 255, 255], "the middle is untouched");
    assert_eq!(rgb_at(&mut d, 5, 5), [255, 255, 255, 255]);
    assert_eq!(d.undo_name(), Some("Stroke"));
    // Color Range picks the red band and nothing else.
    d.deselect();
    d.select_color_range([1.0, 0.0, 0.0], 40.0, true, Mode::Replace).unwrap();
    let sel = d.selection.clone().unwrap();
    assert!(sel.contains(21.0, 30.0) && !sel.contains(30.0, 30.0) && !sel.contains(5.0, 5.0));
    assert!(d.select_color_range([0.0, 0.0, 1.0], 20.0, true, Mode::Replace).is_err(), "nothing blue anywhere");
    // Align three small layers to the canvas, then distribute them.
    d.deselect();
    let a = d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    let b = d.add_shape_layer(false, (5.0, 20.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    let c = d.add_shape_layer(false, (40.0, 45.0, 10.0, 10.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.selected = [a, b, c].into_iter().collect();
    d.align_layers("right").unwrap();
    for id in [a, b, c] { assert_eq!(d.renderer.layer(id).transform.bounds().2, 60.0, "right edges on the canvas edge"); }
    assert_eq!(d.undo_name(), Some("Align Layers"));
    d.distribute_layers(false).unwrap();
    let ys: Vec<f64> = [a, b, c].iter().map(|id| d.renderer.layer(*id).transform.origin.1).collect();
    assert!(ys[0] == 0.0 && ys[2] == 45.0 && (ys[1] == 22.0 || ys[1] == 23.0), "the middle one sits halfway between the outer two: {ys:?}");
    d.selected = [a].into_iter().collect();
    assert!(d.distribute_layers(true).is_err(), "one layer cannot be distributed");
    assert!(d.align_layers("sideways").is_err());
}

#[test]
fn shadows_highlights_selective_color_mixer_high_pass_and_radial_blur() {
    use compositor::filters::{Adjustment, ChannelMixer, Kind, SelectiveColor, Settings, ShadowsHighlights};
    // Shadows lift only the dark side, highlights pull only the bright side.
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 20.0, 40.0), [0.15, 0.15, 0.15], 0.0).unwrap();
    d.add_shape_layer(false, (20.0, 0.0, 20.0, 40.0), [0.9, 0.9, 0.9], 0.0).unwrap();
    let id = d.stamp_visible().unwrap();
    d.select_layer(Some(id));
    let (dark, light) = (rgb_at(&mut d, 5, 20)[0], rgb_at(&mut d, 35, 20)[0]);
    let mut s = Settings::default();
    s.shadows_highlights = ShadowsHighlights { shadows: 60.0, highlights: 0.0, radius: 4.0 };
    d.apply_filter(Kind::ShadowsHighlights, &s).unwrap();
    assert!(rgb_at(&mut d, 5, 20)[0] > dark + 10, "shadows lifted: {}", rgb_at(&mut d, 5, 20)[0]);
    assert!(rgb_at(&mut d, 35, 20)[0] >= light - 2, "highlights left alone");
    d.undo();
    s.shadows_highlights = ShadowsHighlights { shadows: 0.0, highlights: 60.0, radius: 4.0 };
    d.apply_filter(Kind::ShadowsHighlights, &s).unwrap();
    assert!(rgb_at(&mut d, 35, 20)[0] < light - 10, "highlights pulled down");
    assert!(rgb_at(&mut d, 5, 20)[0] <= dark + 2);
    // Selective Color: more cyan in the reds takes red out of a red pixel and leaves a blue one alone.
    let mut sc = SelectiveColor::default();
    sc.set_adjustment("Reds", [60.0, 0.0, 0.0, 0.0]);
    let red = sc.pixel([1.0, 0.1, 0.1]);
    assert!(red[0] < 0.7 && (red[1] - 0.1).abs() < 1e-9, "{red:?}");
    assert_eq!(sc.pixel([0.1, 0.1, 1.0]), [0.1, 0.1, 1.0]);
    let blacks = { let mut b = SelectiveColor::default(); b.set_adjustment("Blacks", [0.0, 0.0, 0.0, 50.0]); b };
    assert!(blacks.pixel([0.2, 0.2, 0.2])[0] < 0.2 && blacks.pixel([0.9, 0.9, 0.9])[0] == 0.9);
    // Channel Mixer: monochrome from the red row, and the identity is the identity.
    assert_eq!(ChannelMixer::default().pixel([0.3, 0.6, 0.9]), [0.3, 0.6, 0.9]);
    let mono = ChannelMixer { red: [100.0, 0.0, 0.0, 0.0], monochrome: true, ..ChannelMixer::default() };
    assert_eq!(mono.pixel([0.3, 0.6, 0.9]), [0.3, 0.3, 0.3]);
    let back = Adjustment::from_record(&Adjustment::SelectiveColor(sc.clone()).to_record()).unwrap();
    assert_eq!(back, Adjustment::SelectiveColor(sc));
    let back = Adjustment::from_record(&Adjustment::ChannelMixer(mono.clone()).to_record()).unwrap();
    assert_eq!(back, Adjustment::ChannelMixer(mono));
    // High Pass: flat areas go to middle gray, the edge keeps a light and a dark side.
    d.undo();
    s.high_pass = 3.0;
    d.apply_filter(Kind::HighPass, &s).unwrap();
    let flat = rgb_at(&mut d, 3, 20)[0];
    assert!((120..=136).contains(&flat), "flat goes gray: {flat}");
    assert!(rgb_at(&mut d, 19, 20)[0] < flat && rgb_at(&mut d, 20, 20)[0] > flat, "the edge stays");
    // Radial Blur (spin) smears a small square around the center; the center pixel itself keeps its color.
    d.undo();
    let mut e = Document::blank(60, 60, 72.0).unwrap();
    e.add_shape_layer(false, (0.0, 0.0, 60.0, 60.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    e.add_shape_layer(false, (40.0, 28.0, 6.0, 4.0), [0.0, 0.0, 0.0], 0.0).unwrap();
    let id = e.stamp_visible().unwrap();
    e.select_layer(Some(id));
    let mut r = Settings::default();
    r.radial.amount = 40.0;
    e.apply_filter(Kind::RadialBlur, &r).unwrap();
    let smeared = rgb_at(&mut e, 42, 26)[0];
    assert!(smeared < 250 && smeared > 60, "the square smears along the arc above it: {smeared}");
    assert_eq!(rgb_at(&mut e, 5, 5)[0], 255, "far corners untouched");
}

#[test]
fn gradient_fill_shapes_and_multi_stop_gradient_map() {
    use compositor::filters::{Adjustment, GradientMap};
    use compositor::gradient::{Gradient, Shape, Stop};
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    // Three stops, linear across: red, then green in the middle, then blue.
    let g = Gradient { stops: vec![Stop { position: 0.0, color: [1.0, 0.0, 0.0] }, Stop { position: 0.5, color: [0.0, 1.0, 0.0] }, Stop { position: 1.0, color: [0.0, 0.0, 1.0] }], alphas: Vec::new() };
    d.gradient_fill((0.0, 20.0), (40.0, 20.0), Shape::Linear, &g, 1.0, true).unwrap();
    assert!(rgb_at(&mut d, 0, 20)[0] > 240);
    let mid = rgb_at(&mut d, 20, 20);
    assert!(mid[1] > 200 && mid[0] < 60 && mid[2] < 60, "green in the middle: {mid:?}");
    assert!(rgb_at(&mut d, 39, 20)[2] > 240);
    assert_eq!(d.undo_name(), Some("Gradient"));
    d.undo();
    // Diamond from the center: the corners of a square at the end distance reach the last color.
    let bw = Gradient::two([0.0; 3], [1.0; 3]);
    d.gradient_fill((20.0, 20.0), (30.0, 20.0), Shape::Diamond, &bw, 1.0, true).unwrap();
    assert!(rgb_at(&mut d, 20, 20)[0] < 30, "dark at the center");
    assert!(rgb_at(&mut d, 30, 30)[0] > 225, "a diamond reaches its corner at the same distance as its edge");
    assert!(rgb_at(&mut d, 25, 20)[0] > 100 && rgb_at(&mut d, 25, 20)[0] < 160, "half way along: {}", rgb_at(&mut d, 25, 20)[0]);
    d.undo();
    // Angle sweeps around the start.
    d.gradient_fill((20.0, 20.0), (40.0, 20.0), Shape::Angle, &bw, 1.0, true).unwrap();
    assert!(rgb_at(&mut d, 35, 20)[0] < 40 || rgb_at(&mut d, 35, 20)[0] > 215, "on the line: the start or the end of the sweep");
    assert!(rgb_at(&mut d, 20, 35)[0] > 40 && rgb_at(&mut d, 20, 35)[0] < 90, "a quarter turn along: {}", rgb_at(&mut d, 20, 35)[0]);
    d.undo();
    // Reflected mirrors across the start.
    d.gradient_fill((20.0, 20.0), (40.0, 20.0), Shape::Reflected, &bw, 1.0, true).unwrap();
    let (left, right, center) = (rgb_at(&mut d, 10, 20)[0], rgb_at(&mut d, 30, 20)[0], rgb_at(&mut d, 20, 20)[0]);
    assert!((left as i32 - right as i32).abs() < 20, "the same tone either side of the start: {left} {right} center {center}");
    // A Gradient Map with three stops maps middle gray to the middle stop, and round-trips.
    let map = GradientMap { stops: g.stops.clone(), ..GradientMap::default() };
    let t = map.table();
    assert!(t[128 * 3 + 1] > 200 && t[128 * 3] < 60, "mid gray hits the green stop");
    assert_eq!(&t[0..3], &[255, 0, 0]);
    let back = Adjustment::from_record(&Adjustment::GradientMap(map.clone()).to_record()).unwrap();
    assert_eq!(back, Adjustment::GradientMap(map));
}

#[test]
fn blend_if_hides_tones_of_this_layer_and_of_what_is_beneath() {
    use compositor::effects::{BlendIf, Effects};
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    // Beneath: dark on the left, light on the right. On top: a mid-gray layer over everything.
    d.add_shape_layer(false, (0.0, 0.0, 20.0, 40.0), [0.1, 0.1, 0.1], 0.0).unwrap();
    d.add_shape_layer(false, (20.0, 0.0, 20.0, 40.0), [0.9, 0.9, 0.9], 0.0).unwrap();
    let top = d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    assert_eq!(rgb_at(&mut d, 5, 20), [255, 0, 0, 255]);
    // Underlying black point at 128 with no feather: the red shows only over the light half.
    let mut e = Effects::default();
    e.blend_if = Some(BlendIf { under_black: 128.0, feather: 0.0, ..BlendIf::default() });
    d.set_effects(top, Some(&e)).unwrap();
    let (left, right) = (rgb_at(&mut d, 5, 20), rgb_at(&mut d, 35, 20));
    assert!(left[0] < 40 && left[1] < 40, "hidden over the dark half: {left:?}");
    assert_eq!(right, [255, 0, 0, 255], "shown over the light half");
    // This Layer's white point below the red's own luminosity (76) hides it everywhere.
    e.blend_if = Some(BlendIf { this_white: 50.0, feather: 0.0, ..BlendIf::default() });
    d.set_effects(top, Some(&e)).unwrap();
    assert!(rgb_at(&mut d, 35, 20)[0] > 200 && rgb_at(&mut d, 35, 20)[1] > 200, "the red is gone, the light gray shows");
    // A feathered edge gives a partial mix.
    e.blend_if = Some(BlendIf { this_white: 76.0, feather: 40.0, ..BlendIf::default() });
    d.set_effects(top, Some(&e)).unwrap();
    let p = rgb_at(&mut d, 35, 20);
    assert!(p[0] > 200 && p[1] > 60 && p[1] < 200, "part red, part gray: {p:?}");
    let back = Effects::from_record(&e.to_record()).unwrap();
    assert_eq!(back.blend_if, e.blend_if);
}

#[test]
fn paragraph_text_wraps_at_its_width_and_shapes_come_from_paths() {
    use compositor::path::{Anchor, Path};
    use compositor::text::TextStyle;
    let mut d = Document::blank(400, 400, 72.0).unwrap();
    let style = TextStyle { text: "one two three four five six seven eight nine ten".into(), size: 20.0, ..TextStyle::default() };
    let single = d.add_text_layer(&style, 10.0, 10.0).unwrap();
    let wide = d.renderer.layer(single).transform.size;
    let boxed = TextStyle { width: Some(120.0), ..style.clone() };
    let para = d.add_text_layer(&boxed, 10.0, 100.0).unwrap();
    let narrow = d.renderer.layer(para).transform.size;
    assert!(narrow.0 <= 130.0 && narrow.1 > wide.1 * 2.5, "wrapped into several lines: {narrow:?} versus {wide:?}");
    assert_eq!(d.text_style(para).unwrap().width, Some(120.0));
    // Back to a single line.
    d.set_text(para, &style).unwrap();
    assert!(d.renderer.layer(para).transform.size.1 < wide.1 * 1.5);
    // A triangle path becomes a shape layer, comes back as a path, and can be replaced.
    let mut path = Path::default();
    path.anchors.push(Anchor::corner((50.0, 50.0)));
    path.anchors.push(Anchor::corner((150.0, 50.0)));
    path.anchors.push(Anchor::corner((100.0, 150.0)));
    path.closed = true;
    let shape = d.add_path_shape_layer(&path, [0.0, 0.0, 1.0]).unwrap();
    let t = d.renderer.layer(shape).transform;
    assert_eq!((t.origin.0, t.origin.1, t.size.0, t.size.1), (50.0, 50.0, 100.0, 100.0));
    assert_eq!(rgb_at(&mut d, 100, 100), [0, 0, 255, 255], "inside the triangle");
    assert_eq!(rgb_at(&mut d, 55, 140)[3], 0, "outside it");
    let back = d.shape_path(shape).unwrap();
    assert_eq!(back.anchors.len(), 3);
    assert_eq!(back.anchors[2].point, (100.0, 150.0));
    // Scale the layer: the path scales with it.
    let mut bigger = t;
    bigger.size = compositor::format::Size(200.0, 200.0);
    d.set_transform(shape, bigger, "Transform Layer");
    assert_eq!(d.shape_path(shape).unwrap().anchors[1].point, (250.0, 50.0));
    assert_eq!(rgb_at(&mut d, 150, 150), [0, 0, 255, 255]);
    // Replace the outline: a square somewhere else.
    let mut square = Path::default();
    for p in [(200.0, 200.0), (300.0, 200.0), (300.0, 300.0), (200.0, 300.0)] { square.anchors.push(Anchor::corner(p)); }
    square.closed = true;
    d.set_shape_path(shape, &square).unwrap();
    let t = d.renderer.layer(shape).transform;
    assert_eq!((t.origin.0, t.origin.1, t.size.0, t.size.1), (200.0, 200.0, 100.0, 100.0));
    assert_eq!(rgb_at(&mut d, 250, 250), [0, 0, 255, 255]);
    assert_eq!(rgb_at(&mut d, 100, 100)[3], 0, "the old triangle is gone");
    assert_eq!(d.undo_name(), Some("Edit Shape"));
    assert!(d.set_shape_path(single, &square).is_err(), "a type layer is not a path shape");
}

#[test]
fn patterns_define_fill_and_stamp() {
    let dir = std::env::temp_dir().join(format!("compy-patterns-{}", std::process::id()));
    let saved_home = std::env::var_os("XDG_DATA_HOME");
    unsafe { std::env::set_var("XDG_DATA_HOME", &dir); }
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 2.0, 2.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.add_shape_layer(false, (2.0, 2.0, 2.0, 2.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    // A 4 x 4 checker of red and white becomes the pattern.
    d.select_box(0.0, 0.0, 4.0, 4.0, false, Mode::Replace, false).unwrap();
    d.define_pattern("Checks").unwrap();
    assert!(compositor::patterns::list().contains(&"Checks".to_string()));
    d.deselect();
    let target = d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.select_layer(Some(target));
    d.fill_pattern("Checks", 1.0, 1.0).unwrap();
    assert_eq!(rgb_at(&mut d, 20, 20), [255, 0, 0, 255], "tiled: (20,20) is a red cell");
    assert_eq!(rgb_at(&mut d, 22, 20), [255, 255, 255, 255], "and (22,20) a white one");
    assert_eq!(d.undo_name(), Some("Fill with Pattern"));
    d.undo();
    assert_eq!(rgb_at(&mut d, 20, 20), [0, 0, 255, 255]);
    // Scaled up twice, the cells are 4 pixels.
    d.fill_pattern("Checks", 2.0, 1.0).unwrap();
    assert_eq!(rgb_at(&mut d, 21, 21), [255, 0, 0, 255]);
    assert_eq!(rgb_at(&mut d, 25, 21), [255, 255, 255, 255]);
    d.undo();
    assert!(d.fill_pattern("Nothing", 1.0, 1.0).is_err());
    // The Pattern Stamp paints the tile through the brush.
    let mut brush = compositor::brush::BrushSettings::default();
    brush.diameter = 12.0;
    d.replay_stroke(&[(20.0, 20.0), (24.0, 20.0)], &brush, compositor::document::StrokeKind::Pattern { name: "Checks".into() }).unwrap();
    assert_eq!(rgb_at(&mut d, 20, 20), [255, 0, 0, 255]);
    assert_eq!(rgb_at(&mut d, 22, 20), [255, 255, 255, 255]);
    assert_eq!(rgb_at(&mut d, 2, 38), [0, 0, 255, 255], "far from the stroke, untouched");
    let _ = std::fs::remove_dir_all(&dir);
    match saved_home { Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) }, None => unsafe { std::env::remove_var("XDG_DATA_HOME") } }
}

#[test]
fn round_seven_fixes_hold() {
    use compositor::effects::{BlendIf, Effects};
    use compositor::filters::{Kind, Settings};
    // A stroke lands on the layer it began on, even when another layer became active meanwhile.
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    let a = d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 1.0, 1.0], 0.0).unwrap();
    let mut brush = compositor::brush::BrushSettings::default();
    brush.diameter = 10.0;
    brush.color = [1.0, 0.0, 0.0];
    d.begin_stroke((20.0, 20.0), &brush, compositor::document::StrokeKind::Paint).unwrap();
    let blue = compositor::raster::new_argb(40, 40).unwrap();
    { let cr = cairo::Context::new(&blue).unwrap(); cr.set_source_rgb(0.0, 0.0, 1.0); cr.paint().unwrap(); }
    let b = d.add_image_surface(blue, "Generated", (0.0, 0.0), (40.0, 40.0)).unwrap();
    assert_eq!(d.active, Some(b));
    d.finish_stroke().unwrap();
    d.set_visible(b, false);
    assert_eq!(rgb_at(&mut d, 20, 20), [255, 0, 0, 255], "the stroke went onto its own layer");
    d.set_visible(b, true);
    d.select_layer(Some(b));
    d.set_visible(a, false);
    assert_eq!(rgb_at(&mut d, 20, 20), [0, 0, 255, 255], "the generated layer is untouched");
    // Blend If reads the same pixels on a scaled (HiDPI) target as on a plain one.
    let mut e = Document::blank(40, 40, 72.0).unwrap();
    e.add_shape_layer(false, (0.0, 0.0, 20.0, 40.0), [0.1, 0.1, 0.1], 0.0).unwrap();
    e.add_shape_layer(false, (20.0, 0.0, 20.0, 40.0), [0.9, 0.9, 0.9], 0.0).unwrap();
    let top = e.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    let mut fx = Effects::default();
    fx.blend_if = Some(BlendIf { under_black: 128.0, feather: 0.0, ..BlendIf::default() });
    e.set_effects(top, Some(&fx)).unwrap();
    let hi = compositor::raster::new_argb(80, 80).unwrap();
    hi.set_device_scale(2.0, 2.0);
    { let cr = cairo::Context::new(&hi).unwrap(); cr.rectangle(0.0, 0.0, 40.0, 40.0); cr.clip(); e.renderer.draw(&cr).unwrap(); }
    let (left, right) = compositor::raster::with_bytes(&hi, |b, stride| { let p = |x: usize, y: usize| { let i = y * stride + x * 4; [b[i + 2], b[i + 1], b[i], b[i + 3]] }; (p(10, 40), p(70, 40)) }).unwrap();
    assert!(left[0] < 40, "hidden over the dark half at 2x: {left:?}");
    assert_eq!(right, [255, 0, 0, 255], "shown over the light half at 2x");
    // A mask stroke at zero opacity changes nothing; at half it paints half.
    let mut m = Document::blank(40, 40, 72.0).unwrap();
    let id = m.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    m.add_mask(false).unwrap();
    m.set_mask_target(true);
    m.select_box(10.0, 10.0, 20.0, 20.0, false, Mode::Replace, false).unwrap();
    m.stroke_selection(2.0, [1.0; 3], 0, 0.0).unwrap();
    let mask_max = |m: &Document| compositor::raster::with_bytes(m.renderer.mask(id).unwrap(), |b, _| b.iter().copied().max().unwrap_or(0)).unwrap();
    assert_eq!(mask_max(&m), 0, "zero opacity leaves the mask black");
    m.stroke_selection(2.0, [1.0; 3], 0, 0.5).unwrap();
    let v = mask_max(&m);
    assert!((120..=136).contains(&v), "half opacity paints half: {v}");
    // Sharpening a flat color against transparency leaves the color alone.
    let mut f = Document::blank(30, 10, 72.0).unwrap();
    let cut = compositor::raster::new_argb(30, 10).unwrap();
    { let cr = cairo::Context::new(&cut).unwrap(); cr.set_source_rgb(0.5, 0.5, 0.5); cr.rectangle(0.0, 0.0, 15.0, 10.0); cr.fill().unwrap(); }
    let layer = f.add_image_surface(cut, "cutout", (0.0, 0.0), (30.0, 10.0)).unwrap();
    f.select_layer(Some(layer));
    let mut s = Settings::default();
    s.sharpen.amount = 100.0;
    s.sharpen.radius = 2.0;
    s.sharpen.threshold = 0.0;
    f.apply_filter(Kind::UnsharpMask, &s).unwrap();
    let edge = rgb_at(&mut f, 14, 5);
    assert!((125..=131).contains(&edge[0]) && edge[3] == 255, "gray stays gray at the cutout's edge: {edge:?}");
    // Define Pattern without a selection takes the layer alone, background left out.
    let dir = std::env::temp_dir().join(format!("compy-patterns-b-{}", std::process::id()));
    let saved = std::env::var_os("XDG_DATA_HOME");
    unsafe { std::env::set_var("XDG_DATA_HOME", &dir); }
    let mut g = Document::blank(20, 20, 72.0).unwrap();
    g.add_shape_layer(false, (0.0, 0.0, 20.0, 20.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    g.add_shape_layer(false, (5.0, 5.0, 4.0, 4.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    g.define_pattern("Alone").unwrap();
    let tile = compositor::patterns::load("Alone").unwrap();
    assert_eq!((tile.width(), tile.height()), (4, 4));
    let px = compositor::raster::with_bytes(&tile, |b, _| [b[2], b[1], b[0], b[3]]).unwrap();
    assert_eq!(px, [255, 0, 0, 255], "the red layer alone, no blue behind it: {px:?}");
    let _ = std::fs::remove_dir_all(&dir);
    match saved { Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) }, None => unsafe { std::env::remove_var("XDG_DATA_HOME") } }
}
