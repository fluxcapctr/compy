//! Generative Fill and Expand: the context window and mask that go out, the layer that comes back, and
//! the canvas growth that sets up an expand. The service itself is not called.

mod common;

use common::*;
use compositor::document::Document;
use compositor::genfill::{self, Backend};
use compositor::selection::Mode;

fn flat(d: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; let a = p[3] as u32; let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 }; [un(p[2]), un(p[1]), un(p[0]), p[3]] }).collect()).unwrap();
    (px, w as u32)
}

fn png_pixels(png: &[u8]) -> (Vec<[u8; 4]>, usize, usize) {
    let (s, w, h) = Document::decode_image_bytes(png).unwrap();
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; [p[2], p[1], p[0], p[3]] }).collect()).unwrap();
    (px, w, h)
}

#[test]
fn the_window_wraps_the_selection_with_context_and_a_white_mask() {
    let mut f = Fixture::new("genfill-window", 400, 300);
    let id = f.add(Spec { size: (400.0, 300.0), pixels: Some(halves(400, 300, 200, RED, BLUE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    assert!(d.genfill_window().is_err(), "needs a selection");
    d.select_box(180.0, 100.0, 40.0, 40.0, false, Mode::Replace, false).unwrap();
    // 40 px selection: context is at least 96 px each way.
    assert_eq!(d.genfill_window().unwrap(), (84, 4, 316, 236));
    let (image, mask, window, scale) = d.genfill_inputs(true).unwrap();
    assert_eq!(window, (84, 4, 316, 236));
    assert_eq!(scale, 1.0);
    let (px, w, h) = png_pixels(&image);
    assert_eq!((w, h), (232, 232));
    assert_eq!(px[10 * w + 10][..3], [255, 0, 0], "red side of the composite");
    assert_eq!(px[10 * w + 220][..3], [0, 0, 255], "blue side");
    let (m, _, _) = png_pixels(&mask);
    assert_eq!(m[10 * w + 10][..3], [0, 0, 0], "outside the selection is black");
    assert_eq!(m[110 * w + 110][..3], [255, 255, 255], "the selection is white");
    // A selection at the canvas edge is clipped to it; a large one scales down to the model's limit.
    d.select_box(0.0, 0.0, 300.0, 300.0, false, Mode::Replace, false).unwrap();
    let (image, _, window, scale) = d.genfill_inputs(false).unwrap();
    assert_eq!(window, (0, 0, 400, 300));
    assert_eq!(scale, 1.0);
    let (_, w, _) = png_pixels(&image);
    assert_eq!(w, 400);
    let mut g = Fixture::new("genfill-big", 4000, 1000);
    let big = g.add(Spec { size: (4000.0, 1000.0), pixels: Some(solid(40, 10, RED)), ..Default::default() });
    let mut d2 = Document::new(g.load().unwrap()).unwrap();
    d2.active = Some(big);
    d2.select_all().unwrap();
    let (image, mask, _, scale) = d2.genfill_inputs(true).unwrap();
    assert!((scale - genfill::MAX_SIDE as f64 / 4000.0).abs() < 1e-9);
    let (_, w, h) = png_pixels(&image);
    assert_eq!((w, h), (genfill::MAX_SIDE, 384));
    let (_, mw, mh) = png_pixels(&mask);
    assert_eq!((mw, mh), (w, h), "the mask matches the image's size");
}

#[test]
fn results_land_as_a_masked_layer_and_variations_replace_it() {
    let mut f = Fixture::new("genfill-apply", 100, 100);
    let id = f.add(Spec { size: (100.0, 100.0), pixels: Some(solid(100, 100, WHITE)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.select_box(40.0, 40.0, 20.0, 20.0, false, Mode::Replace, false).unwrap();
    let (_, _, window, _) = d.genfill_inputs(true).unwrap();
    // A "result" the size the model would return: green everywhere.
    let (w, h) = (window.2 - window.0, window.3 - window.1);
    let green = compositor::png_io::png_bytes(&compositor::png_io::from_straight_rgba(&vec![[0u8, 255, 0, 255]; (w * h) as usize].concat(), w as usize, h as usize).unwrap()).unwrap();
    let layer = d.apply_genfill(&green, window, "Generative Fill").unwrap();
    assert_eq!(d.renderer.layers().len(), 2);
    assert_eq!(d.renderer.layer(layer).name, "Generative Fill 1");
    assert!(d.renderer.mask(layer).is_some(), "shown through the selection");
    let (px, pw) = flat(&mut d);
    assert_pixel(&px, pw, 50, 50, [0, 255, 0, 255], 0);
    assert_pixel(&px, pw, 10, 10, WHITE, 0, );
    assert_pixel(&px, pw, 30, 50, WHITE, 0, );
    assert_eq!(d.undo_name(), Some("Generative Fill"));
    // Another variation swaps the pixels in place.
    let blue = compositor::png_io::png_bytes(&compositor::png_io::from_straight_rgba(&vec![[0u8, 0, 255, 255]; (w * h) as usize].concat(), w as usize, h as usize).unwrap()).unwrap();
    d.replace_genfill(layer, &blue, window).unwrap();
    let (px, pw) = flat(&mut d);
    assert_pixel(&px, pw, 50, 50, BLUE, 0);
    assert_eq!(d.renderer.layers().len(), 2);
    d.undo();
    let (px, pw) = flat(&mut d);
    assert_pixel(&px, pw, 50, 50, [0, 255, 0, 255], 0);
}

#[test]
fn expand_grows_the_canvas_and_selects_the_margin() {
    let mut f = Fixture::new("genfill-expand", 40, 30);
    let id = f.add(Spec { size: (40.0, 30.0), pixels: Some(solid(40, 30, RED)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    assert!(d.expand_canvas_for_fill(20, 30, 4).is_err(), "only grows");
    d.expand_canvas_for_fill(80, 30, 4).unwrap();
    assert_eq!((d.width(), d.height()), (80, 30));
    assert_eq!(d.renderer.layer(id).transform.origin.0, 20.0, "centered");
    let s = d.selection.as_ref().unwrap();
    assert!(s.contains(5.0, 5.0) && s.contains(75.0, 5.0) && !s.contains(40.0, 15.0), "the margins are selected, the picture is not");
    assert_eq!(d.genfill_window().unwrap(), (0, 0, 80, 30));
    assert_eq!(d.undo_name(), Some("Generative Expand"));
    d.undo();
    assert_eq!(d.width(), 40);
}

#[test]
fn a_fake_backend_drives_the_flow() {
    struct Fake;
    impl genfill::Backend for Fake {
        fn generate(&self, request: &genfill::Request, progress: &dyn Fn(&str), _: &dyn Fn() -> bool) -> anyhow::Result<Vec<Vec<u8>>> {
            progress("faking");
            let (_, w, h) = png_pixels(&request.image_png);
            let png = compositor::png_io::png_bytes(&compositor::png_io::from_straight_rgba(&vec![[255u8, 0, 255, 255]; w * h].concat(), w, h).unwrap()).unwrap();
            Ok(vec![png; request.count as usize])
        }
    }
    let mut d = Document::blank(64, 64, 72.0).unwrap();
    d.fill([1.0, 1.0, 1.0]).unwrap();
    d.select_box(16.0, 16.0, 32.0, 32.0, false, Mode::Replace, false).unwrap();
    let (image_png, mask_png, window, _) = d.genfill_inputs(true).unwrap();
    let request = genfill::Request { model: genfill::default_models()[0].clone(), prompt: "a cat".into(), count: 2, seed: None, image_png, mask_png, expand: None };
    let results = Fake.generate(&request, &|_| {}, &|| false).unwrap();
    assert_eq!(results.len(), 2);
    d.apply_genfill(&results[0], window, "Generative Fill").unwrap();
    let (px, pw) = flat(&mut d);
    assert_pixel(&px, pw, 32, 32, [255, 0, 255, 255], 0);
    assert_pixel(&px, pw, 2, 2, WHITE, 0);
    assert!(genfill::key().is_none() || !genfill::key().unwrap().is_empty());
}

#[test]
fn a_result_that_drifted_in_tone_is_matched_to_the_picture() {
    // The picture is mid grey; the "model" hands back the window 20 levels warmer, with a dark square
    // painted in the hole. The drift comes out; what was painted stays darker than the picture.
    let mut f = Fixture::new("genfill-tone", 200, 200);
    let id = f.add(Spec { size: (200.0, 200.0), pixels: Some(solid(200, 200, [128, 128, 128, 255])), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    d.select_box(70.0, 70.0, 60.0, 60.0, false, Mode::Replace, false).unwrap();
    let (_, _, window, _) = d.genfill_inputs(true).unwrap();
    let (w, h) = ((window.2 - window.0) as usize, (window.3 - window.1) as usize);
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h { for x in 0..w {
        let (dx, dy) = (x as i32 + window.0, y as i32 + window.1);
        let inside = (90..110).contains(&dx) && (90..110).contains(&dy);
        px.extend_from_slice(&if inside { [60u8, 40, 40, 255] } else { [148u8, 138, 128, 255] });
    } }
    let png = compositor::png_io::png_bytes(&compositor::png_io::from_straight_rgba(&px, w, h).unwrap()).unwrap();
    d.apply_genfill(&png, window, "Generative Fill").unwrap();
    let (flat_px, pw) = flat(&mut d);
    assert_pixel(&flat_px, pw, 75, 100, [128, 128, 128, 255], 3);
    assert_pixel(&flat_px, pw, 100, 75, [128, 128, 128, 255], 3);
    assert_pixel(&flat_px, pw, 100, 100, [40, 30, 40, 255], 4);
}
