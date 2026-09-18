mod common;
use common::*;
use compositor::brush::BrushSettings;
use compositor::document::{Document, StrokeKind};
use compositor::raster::with_bytes;
use compositor::selection::Selection;
use uuid::Uuid;

fn doc(f: &Fixture) -> Document { Document::new(f.load().unwrap()).unwrap() }

/// Straight-alpha RGBA of the flattened document.
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

fn brush(diameter: f64, hardness: f64, opacity: f64) -> BrushSettings { BrushSettings { diameter, hardness, color: [1.0, 0.0, 0.0], opacity } }

fn stroke(d: &mut Document, points: &[(f64, f64)], settings: &BrushSettings, kind: StrokeKind) {
    d.begin_stroke(points[0], settings, kind).unwrap();
    for p in &points[1..] { d.continue_stroke(*p).unwrap(); }
    d.finish_stroke().unwrap();
}

#[test]
fn a_dab_on_a_blank_layer_paints_a_disk_and_shrinks_the_layer_to_it() {
    let mut f = Fixture::new("dab", 16, 16);
    let id: Uuid = f.add(Spec { name: "Blank", size: (16.0, 16.0), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(8.0, 8.0)], &brush(6.0, 1.0, 1.0), StrokeKind::Paint);
    let px = flat(&mut d);
    assert_pixel(&px, 16, 8, 8, RED, 0);
    assert_pixel(&px, 16, 7, 7, RED, 0);
    assert_pixel(&px, 16, 1, 1, CLEAR, 0);
    assert_pixel(&px, 16, 8, 13, CLEAR, 0);
    // The layer keeps only the painted area, padded out to the 64-pixel halving alignment (a transparent
    // margin the Mac app does not keep, in exchange for a mouse-up that costs nothing on huge layers).
    let (w, h) = d.renderer.image_size(id).unwrap();
    assert!((6..=64).contains(&w) && (6..=64).contains(&h), "layer is {w}x{h}");
    let t = d.renderer.layer(id).transform;
    assert!(t.origin.0 <= 5.0 && t.origin.1 <= 5.0 && t.origin.0 + t.size.0 >= 11.0 && t.origin.1 + t.size.1 >= 11.0, "placed {:?} {:?}", t.origin, t.size);
    assert_eq!(d.undo_name(), Some("Brush Stroke"));
    assert!(d.undo());
    assert!(flat(&mut d).iter().all(|p| *p == CLEAR));
}

#[test]
fn opacity_caps_the_whole_stroke() {
    let mut f = Fixture::new("cap", 32, 8);
    let id = f.add(Spec { name: "Blank", size: (32.0, 8.0), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(4.0, 4.0), (12.0, 4.0), (20.0, 4.0), (28.0, 4.0)], &brush(6.0, 1.0, 0.5), StrokeKind::Paint);
    let px = flat(&mut d);
    let along: Vec<u8> = (4..28).map(|x| px[4 * 32 + x][3]).collect();
    assert!(along.iter().all(|a| (126..=129).contains(a)), "alpha along the stroke: {along:?}");
}

#[test]
fn erasing_clears_and_a_selection_limits_it() {
    let mut f = Fixture::new("erase", 8, 8);
    let id = f.add(Spec { name: "Red", size: (8.0, 8.0), pixels: Some(solid(8, 8, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(4.0, 4.0)], &brush(4.0, 1.0, 1.0), StrokeKind::Erase);
    let px = flat(&mut d);
    assert_pixel(&px, 8, 4, 4, CLEAR, 0);
    assert_pixel(&px, 8, 0, 0, RED, 0);
    d.undo();
    let left = Selection::from_shape(8, 8, false, |cr| { cr.rectangle(0.0, 0.0, 4.0, 8.0); cr.fill()?; Ok(()) }).unwrap();
    d.set_selection(Some(left), "Marquee");
    stroke(&mut d, &[(4.0, 4.0)], &brush(40.0, 1.0, 1.0), StrokeKind::Erase);
    let px = flat(&mut d);
    for y in 0..8 { for x in 0..8 { assert_pixel(&px, 8, x, y, if x < 4 { CLEAR } else { RED }, 0); } }
}

#[test]
fn painting_past_the_edge_grows_the_layer() {
    let mut f = Fixture::new("grow", 12, 12);
    let id = f.add(Spec { name: "Red", origin: (4.0, 4.0), size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let mut blue = brush(3.0, 1.0, 1.0);
    blue.color = [0.0, 0.0, 1.0];
    stroke(&mut d, &[(1.0, 1.0)], &blue, StrokeKind::Paint);
    let px = flat(&mut d);
    assert_pixel(&px, 12, 1, 1, BLUE, 0);
    assert_pixel(&px, 12, 5, 5, RED, 0);
    assert_pixel(&px, 12, 10, 10, CLEAR, 0);
    let t = d.renderer.layer(id).transform;
    assert!(t.origin.0 <= 1.0 && t.origin.1 <= 1.0, "origin {:?}", t.origin);
    let (w, h) = d.renderer.image_size(id).unwrap();
    assert!(w >= 7 && h >= 7, "grew to {w}x{h}");
    d.undo();
    assert_eq!(d.renderer.layer(id).transform.origin.0, 4.0);
    assert_eq!(d.renderer.image_size(id), Some((4, 4)));
}

#[test]
fn a_soft_brush_fades_to_its_rim() {
    let mut f = Fixture::new("soft", 24, 24);
    let id = f.add(Spec { name: "Blank", size: (24.0, 24.0), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(12.0, 12.0)], &brush(16.0, 0.0, 1.0), StrokeKind::Paint);
    let px = flat(&mut d);
    let center = px[12 * 24 + 12][3];
    let mid = px[12 * 24 + 16][3];
    let rim = px[12 * 24 + 19][3];
    assert!(center >= 250, "center {center}");
    assert!(mid > rim && rim > 0 && mid < center, "center {center} mid {mid} rim {rim}");
    assert_eq!(px[12 * 24 + 22][3], 0);
}

#[test]
fn clone_stamp_copies_from_the_offset() {
    let mut f = Fixture::new("clone", 8, 8);
    let id = f.add(Spec { name: "Halves", size: (8.0, 8.0), pixels: Some(halves(8, 8, 4, RED, BLUE)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(6.0, 2.0), (6.0, 6.0)], &brush(3.0, 1.0, 1.0), StrokeKind::Clone { offset: (-4.0, 0.0), all_layers: false });
    let px = flat(&mut d);
    assert_pixel(&px, 8, 6, 4, RED, 0);
    assert_pixel(&px, 8, 2, 4, RED, 0);
    assert_pixel(&px, 8, 7, 0, BLUE, 0);
    assert_eq!(d.undo_name(), Some("Clone Stamp"));
}

#[test]
fn blur_softens_an_edge_under_the_tip() {
    let mut f = Fixture::new("blur", 16, 16);
    let id = f.add(Spec { name: "Halves", size: (16.0, 16.0), pixels: Some(halves(16, 16, 8, RED, BLUE)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(8.0, 8.0)], &brush(20.0, 1.0, 1.0), StrokeKind::Blur);
    let px = flat(&mut d);
    let left = px[8 * 16 + 7];
    let right = px[8 * 16 + 8];
    assert!(left[0] < 255 && left[2] > 0, "edge pixel blurred: {left:?}");
    assert!(right[0] > 0 && right[2] < 255, "edge pixel blurred: {right:?}");
    assert_pixel(&px, 16, 0, 0, RED, 0);
    assert_eq!(d.undo_name(), Some("Blur"));
}

#[test]
fn spot_healing_brush_repairs_a_spot() {
    let mut f = Fixture::new("heal-brush", 16, 16);
    let mut pixels = vec![RED; 256];
    for y in 7..9 { for x in 7..9 { pixels[y * 16 + x] = BLUE; } }
    let id = f.add(Spec { name: "Spot", size: (16.0, 16.0), pixels: Some((16, 16, pixels)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    stroke(&mut d, &[(8.0, 8.0)], &brush(5.0, 1.0, 1.0), StrokeKind::Heal { mode: 1 });
    let px = flat(&mut d);
    for y in 7..9 { for x in 7..9 { let p = px[y * 16 + x]; assert!(p[0] > 200 && p[2] < 64, "healed {p:?}"); } }
    assert_pixel(&px, 16, 0, 0, RED, 0);
    assert_eq!(d.undo_name(), Some("Spot Healing"));
}

#[test]
fn the_live_preview_matches_the_commit() {
    let mut f = Fixture::new("preview-stroke", 64, 64);
    let id = f.add(Spec { name: "Blank", size: (64.0, 64.0), sampling: "High quality", ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.begin_stroke((10.0, 10.0), &brush(8.0, 0.5, 1.0), StrokeKind::Paint).unwrap();
    d.continue_stroke((50.0, 40.0)).unwrap();
    d.continue_stroke((30.0, 55.0)).unwrap();
    let during = flat(&mut d);
    d.finish_stroke().unwrap();
    let after = flat(&mut d);
    assert!(during.iter().any(|p| p[3] > 0));
    let differing = during.iter().zip(&after).filter(|(a, b)| !close(**a, **b, 2)).count();
    // The provisional straight tail becomes a curve on release, so a few percent of pixels may move.
    assert!(differing <= 64 * 64 / 12, "preview and commit differ in {differing} pixels");
}
