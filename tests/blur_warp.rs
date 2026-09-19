mod common;
use common::*;
use compositor::brush::BrushSettings;
use compositor::document::Document;
use compositor::filters::{Kind, Settings};
use compositor::raster::with_bytes;
use compositor::warp::WarpMode;

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

#[test]
fn gaussian_blur_softens_and_grows_the_layer() {
    let mut f = Fixture::new("gauss", 24, 24);
    let id = f.add(Spec { name: "Halves", origin: (4.0, 4.0), size: (16.0, 16.0), pixels: Some(halves(16, 16, 8, RED, BLUE)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.apply_filter(Kind::GaussianBlur, &Settings { radius: 1.0, ..Settings::default() }).unwrap();
    let px = flat(&mut d);
    let edge = px[12 * 24 + 11];
    assert!(edge[0] > 40 && edge[2] > 40, "the edge is a mix: {edge:?}");
    assert_pixel(&px, 24, 8, 12, RED, 12);
    assert!(px[12 * 24 + 3][3] > 0 && px[12 * 24 + 3][3] < 255, "the blur spread past the old edge: {:?}", px[12 * 24 + 3]);
    let t = d.renderer.layer(id).transform;
    assert!(t.origin.0 < 4.0 && t.size.0 > 16.0, "layer grew: {t:?}");
    assert_eq!(d.undo_name(), Some("Gaussian Blur"));
    d.undo();
    assert_eq!(d.renderer.layer(id).transform.origin.0, 4.0);
}

#[test]
fn motion_blur_streaks_along_its_angle() {
    let mut f = Fixture::new("motion", 32, 32);
    let mut pixels = vec![RED; 32 * 32];
    for y in 0..32 { for x in 0..32 { if x >= 16 { pixels[y * 32 + x] = BLUE; } } }
    let id = f.add(Spec { name: "Vertical edge", size: (32.0, 32.0), pixels: Some((32, 32, pixels)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.apply_filter(Kind::MotionBlur, &Settings { angle: 0.0, distance: 8.0, ..Settings::default() }).unwrap();
    let px = flat(&mut d);
    let near = px[16 * 32 + 14];
    assert!(near[0] > 60 && near[2] > 60, "a horizontal streak mixes across a vertical edge: {near:?}");
    d.undo();
    d.apply_filter(Kind::MotionBlur, &Settings { angle: 90.0, distance: 8.0, ..Settings::default() }).unwrap();
    let px = flat(&mut d);
    assert_pixel(&px, 32, 14, 16, RED, 4);
    assert_pixel(&px, 32, 17, 16, BLUE, 4);
    assert_eq!(d.undo_name(), Some("Motion Blur"));
}

#[test]
fn smudge_drags_color_and_liquify_pushes_pixels() {
    let mut f = Fixture::new("smudge", 24, 24);
    let id = f.add(Spec { name: "Halves", size: (24.0, 24.0), pixels: Some(halves(24, 24, 12, RED, BLUE)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let settings = BrushSettings { diameter: 6.0, hardness: 0.5, color: [0.0; 3], opacity: 0.9, ..Default::default() };
    d.begin_warp((6.0, 12.0), &settings, WarpMode::Smudge).unwrap();
    for x in 7..=18 { d.continue_stroke((x as f64, 12.0)).unwrap(); }
    let during = flat(&mut d);
    assert!(during[12 * 24 + 15][0] > 60, "the live preview shows red dragged into the blue: {:?}", during[12 * 24 + 15]);
    d.finish_stroke().unwrap();
    let px = flat(&mut d);
    assert!(px[12 * 24 + 15][0] > 60, "red dragged into the blue: {:?}", px[12 * 24 + 15]);
    assert_pixel(&px, 24, 2, 2, RED, 0);
    assert_pixel(&px, 24, 21, 21, BLUE, 0);
    assert_eq!(d.undo_name(), Some("Smudge"));
    d.undo();
    assert_pixel(&flat(&mut d), 24, 15, 12, BLUE, 0);

    let settings = BrushSettings { diameter: 8.0, hardness: 0.0, color: [0.0; 3], opacity: 1.0, ..Default::default() };
    d.begin_warp((11.0, 12.0), &settings, WarpMode::Liquify).unwrap();
    for x in 12..=16 { d.continue_stroke((x as f64, 12.0)).unwrap(); }
    d.finish_stroke().unwrap();
    let px = flat(&mut d);
    assert!(px[12 * 24 + 13][0] > 150, "the edge was pushed right: {:?}", px[12 * 24 + 13]);
    assert_pixel(&px, 24, 12, 2, BLUE, 0);
    assert_eq!(d.undo_name(), Some("Liquify"));
}
