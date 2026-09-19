//! Tools: the eyedropper, gradients on layers and masks, shape layers that redraw when scaled, and crop.

mod common;

use common::*;
use compositor::document::Document;
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
fn eyedropper_reads_the_composite_or_the_layer() {
    let mut f = Fixture::new("eyedropper", 4, 4);
    let below = f.add(Spec { size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Default::default() });
    let above = f.add(Spec { origin: (2.0, 0.0), size: (2.0, 4.0), pixels: Some(solid(2, 4, [0, 0, 255, 128])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(below);
    assert_eq!(d.sample_color(0.5, 0.5, true).unwrap(), Some([1.0, 0.0, 0.0]));
    let mixed = d.sample_color(3.5, 0.5, true).unwrap().unwrap();
    assert!(mixed[0] > 0.4 && mixed[0] < 0.6 && mixed[2] > 0.4 && mixed[2] < 0.6, "half blue over red: {mixed:?}");
    assert_eq!(d.sample_color(3.5, 0.5, false).unwrap(), Some([1.0, 0.0, 0.0]), "the active layer alone");
    d.active = Some(above);
    assert_eq!(d.sample_color(0.5, 0.5, false).unwrap(), None, "transparent on the layer");
    assert_eq!(d.sample_color(9.0, 9.0, true).unwrap(), None, "outside the canvas");
}

#[test]
fn gradients_fill_layers_and_masks_inside_the_selection() {
    let mut f = Fixture::new("gradient", 8, 2);
    let id = f.add(Spec { size: (8.0, 2.0), pixels: Some(solid(8, 2, WHITE)), mask: Some((8, 2, vec![255; 16])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    let colors = [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    d.gradient((0.0, 1.0), (8.0, 1.0), false, colors, 1.0, false).unwrap();
    d.clear_preview();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, WHITE, 0);
    d.gradient((0.0, 1.0), (8.0, 1.0), false, colors, 1.0, true).unwrap();
    assert_eq!(d.undo_name(), Some("Gradient"));
    let (px, w) = flat(&mut d);
    assert!(px[0][0] > 200 && px[0][2] < 60, "red end: {:?}", px[0]);
    assert!(px[7][2] > 200 && px[7][0] < 60, "blue end: {:?}", px[7]);
    assert!(px[4][0] > 60 && px[4][2] > 60, "mixed in the middle: {:?}", px[4]);
    // Foreground to transparent at half opacity inside a selection leaves the rest alone.
    d.undo();
    d.select_box(0.0, 0.0, 4.0, 2.0, false, Mode::Replace, false).unwrap();
    d.gradient((0.0, 1.0), (4.0, 1.0), false, [[1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0]], 0.5, true).unwrap();
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 6, 0, WHITE, 0);
    assert!(px[0][1] > 100 && px[0][1] < 160, "half red over white at the start: {:?}", px[0]);
    // Radial on the mask: white at the center fading to black at the rim.
    d.deselect();
    d.set_mask_target(true);
    d.gradient((4.0, 1.0), (8.0, 1.0), true, [[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]], 1.0, true).unwrap();
    assert_eq!(d.undo_name(), Some("Gradient Mask"));
    let m = mask_bytes(&d, id);
    assert!(m[4] > 200 && m[0] < 40 && m[7] < 60, "{:?}", &m[..8]);
    assert!(m[5] > m[6] && m[6] > m[7]);
}

#[test]
fn shape_layers_draw_and_redraw_when_scaled() {
    let mut d = Document::blank(20, 20, 72.0).unwrap();
    let id = d.add_shape_layer(false, (2.0, 2.0, 10.0, 6.0), [0.0, 0.0, 1.0], 3.0).unwrap();
    assert_eq!(d.renderer.layers().len(), 2);
    let layer = d.renderer.layer(id).clone();
    assert_eq!(layer.name, "Rectangle 1");
    assert_eq!((layer.transform.origin.0, layer.transform.size.0, layer.transform.size.1), (2.0, 10.0, 6.0));
    assert_eq!(layer.shape.as_ref().unwrap()["kind"], "Rectangle");
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 6, 4, BLUE, 0);
    assert!(px[2 * 20 + 2][3] < 40, "the rounded corner is clear: {:?}", px[2 * 20 + 2]);
    assert_eq!(d.undo_name(), Some("Rectangle"));
    // Scaling redraws the shape at the new size: the corner radius stays 3, not stretched.
    let mut t = layer.transform;
    t.size = compositor::format::Size(20.0, 6.0);
    t.origin = compositor::format::Point(0.0, 2.0);
    d.set_transform(id, t, "Scale Layer");
    assert_eq!(d.renderer.image(id).unwrap().width(), 20, "redrawn at 20 wide");
    assert!(d.renderer.layer(id).shape.is_some());
    let (px, w) = flat(&mut d);
    assert!(px[2 * 20][3] < 40, "the corner stays rounded after scaling: {:?}", px[2 * 20]);
    assert_pixel(&px, w, 3, 2, BLUE, 40, );
    // Painting over it turns the layer into plain pixels.
    d.fill([1.0, 0.0, 0.0]).unwrap();
    assert!(d.renderer.layer(id).shape.is_none());
    let e = d.add_shape_layer(true, (0.0, 0.0, 10.0, 10.0), [0.0, 1.0, 0.0], 0.0).unwrap();
    assert_eq!(d.renderer.layer(e).name, "Ellipse 1");
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 5, 5, [0, 255, 0, 255], 0);
    assert!(d.add_shape_layer(false, (0.0, 0.0, 0.0, 5.0), [0.0; 3], 0.0).is_err());
}

#[test]
fn crop_cuts_the_canvas_to_the_frame() {
    let mut f = Fixture::new("crop", 8, 8);
    let id = f.add(Spec { size: (8.0, 8.0), pixels: Some(halves(8, 8, 4, RED, BLUE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.select_all().unwrap();
    d.crop((3.0, 2.0, 4.0, 4.0)).unwrap();
    assert_eq!((d.width(), d.height()), (4, 4));
    assert_eq!(d.renderer.layer(id).transform.origin.0, -3.0);
    assert!(d.selection.is_none());
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 0, 0, RED, 0);
    assert_pixel(&px, w, 1, 0, BLUE, 0);
    assert_eq!(d.undo_name(), Some("Crop"));
    d.undo();
    assert_eq!(d.width(), 8);
    assert!(d.crop((0.0, 0.0, 0.0, 4.0)).is_err());
    let (xs, ys) = d.crop_snap_targets();
    assert!(xs.contains(&0.0) && xs.contains(&8.0) && ys.contains(&8.0));
}
