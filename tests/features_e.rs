//! HEIC import, and background removal end to end (the model run is ignored unless the model is present).

mod common;

use common::*;
use compositor::document::Document;
use compositor::matte::{self, MatteSettings};
use std::path::{Path, PathBuf};

fn flat(d: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; let a = p[3] as u32; let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 }; [un(p[2]), un(p[1]), un(p[0]), p[3]] }).collect()).unwrap();
    (px, w as u32)
}
fn fixture(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name) }

#[test]
fn heic_opens_as_a_document() {
    let mut d = Document::open_image(&fixture("red-20x10.heic")).unwrap();
    assert_eq!((d.width(), d.height()), (20, 10));
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 5, 5, RED, 12);
    assert!(compositor::heic::is_heic(Path::new("x.HEIC")) && !compositor::heic::is_heic(Path::new("x.png")));
    let message = match compositor::heic::decode(&fixture("psd/missing-channels.psd")) { Err(e) => e.to_string(), Ok(_) => panic!("opened") };
    assert!(!message.is_empty());
}

#[test]
fn remove_background_refuses_without_the_model_and_refines_with_settings() {
    if matte::model_ready() { return; }
    let mut d = Document::blank(8, 8, 72.0).unwrap();
    d.fill([1.0, 0.0, 0.0]).unwrap();
    let message = match d.remove_background(&MatteSettings::default(), false) { Err(e) => e.to_string(), Ok(()) => panic!("ran without a model") };
    assert!(message.contains("not downloaded"), "{message}");
}

/// Needs the model on disk (Filter > Remove Background downloads it); run with `cargo test -- --ignored`.
#[test]
#[ignore]
fn remove_background_finds_a_subject() {
    if !matte::model_ready() { matte::download_model(|_, _| {}).unwrap(); }
    // A dark disc on a light ground.
    let mut f = Fixture::new("matte", 256, 256);
    let pixels: Vec<[u8; 4]> = (0..256 * 256).map(|i| { let (x, y) = ((i % 256) as f64 - 128.0, (i / 256) as f64 - 128.0); if x.hypot(y) < 60.0 { [30, 30, 30, 255] } else { [235, 235, 235, 255] } }).collect();
    let id = f.add(Spec { size: (256.0, 256.0), pixels: Some((256, 256, pixels)), ..Default::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    d.active = Some(id);
    let started = std::time::Instant::now();
    d.remove_background(&MatteSettings { advanced: true, ..Default::default() }, true).unwrap();
    eprintln!("remove background took {:?}", started.elapsed());
    assert_eq!(d.undo_name(), Some("Remove Background"));
    let mask = d.renderer.mask(id).unwrap();
    let m = compositor::raster::with_bytes(mask, |b, stride| (b[128 * stride + 128], b[8 * stride + 8])).unwrap();
    assert!(m.0 > 200 && m.1 < 60, "center {} corner {}", m.0, m.1);
    let (px, w) = flat(&mut d);
    assert_pixel(&px, w, 8, 8, CLEAR, 60, );
}
