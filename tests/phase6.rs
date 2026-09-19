mod common;
use common::*;
use compositor::document::Document;
use compositor::filters::{self, Adjustment};
use compositor::format;
use compositor::raster::with_bytes;
use compositor::selection::Mode;

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
fn gray(v: u8) -> [u8; 4] { [v, v, v, 255] }
fn temp(name: &str) -> std::path::PathBuf { std::env::temp_dir().join(format!("compositor-p6-{}-{name}", std::process::id())) }

#[test]
fn a_saved_project_reopens_the_same() {
    let mut f = Fixture::new("save", 6, 4);
    let base = f.add(Spec { name: "Base", size: (6.0, 4.0), pixels: Some(halves(6, 4, 3, RED, BLUE)), mask: Some((6, 4, vec![255; 24])), ..Spec::default() });
    let folder = f.add(Spec { name: "Folder", group: true, size: (6.0, 4.0), ..Spec::default() });
    f.add(Spec { name: "Top", origin: (1.0, 1.0), size: (2.0, 2.0), rotation: 30.0, flip: (true, false), pixels: Some(solid(2, 2, WHITE)), parent: Some(folder), opacity: Some(0.5), blend: Some("Multiply"), mask_source: None, ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(base);
    d.add_adjustment("Levels");
    let before = flat(&mut d);
    let path = temp("roundtrip.comp");
    d.save(&path).unwrap();
    assert!(!d.is_modified());
    assert!(path.join("manifest.json").exists());
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(json["version"], 7);
    assert!(json["layers"][0]["id"].as_str().unwrap().chars().all(|c| !c.is_ascii_lowercase()), "ids are uppercase like the Mac's");
    let mut reopened = Document::new(format::load(&path).unwrap()).unwrap();
    assert_eq!(flat(&mut reopened), before);
    let layers = reopened.renderer.layers();
    assert_eq!(layers.len(), 4);
    assert!(layers[3].adjustment.is_some() || layers.iter().any(|l| l.adjustment.is_some()));
    let top = layers.iter().find(|l| l.name == "Top").unwrap();
    assert_eq!((top.transform.rotation, top.transform.flip_x, top.opacity(), top.blend_mode().name()), (30.0, true, 0.5, "Multiply"));
    // Saving again over the existing package replaces it cleanly.
    reopened.save(&path).unwrap();
    assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains("roundtrip.comp")).count(), 1);
    std::fs::remove_dir_all(&path).unwrap();
}

#[test]
fn blank_documents_and_layer_operations() {
    let mut d = Document::blank(4, 4, 150.0).unwrap();
    assert_eq!((d.width(), d.height(), d.renderer.resolution()), (4, 4, 150.0));
    assert_eq!(d.renderer.layers().len(), 1);
    let first = d.active.unwrap();
    let second = d.add_blank_layer();
    assert_eq!(d.renderer.layers()[1].id, second);
    assert_eq!(d.renderer.layer(second).name, "Layer 2");
    d.rename_layer(second, "Sky");
    assert_eq!(d.renderer.layer(second).name, "Sky");
    d.active = Some(first);
    d.move_layer(true);
    assert_eq!(d.renderer.layers()[1].id, first, "moved above Sky");
    let copy = d.duplicate_layer().unwrap();
    assert_eq!(d.renderer.layer(copy).name, "Layer 1 copy");
    assert_eq!(d.renderer.layers().len(), 3);
    let folder = d.add_folder();
    assert!(d.renderer.layer(folder).is_group());
    d.active = Some(copy);
    d.delete_layer();
    assert!(d.renderer.layers().iter().all(|l| l.id != copy));
    assert!(d.undo());
    assert!(d.renderer.layers().iter().any(|l| l.id == copy));
    assert_eq!(d.undo_name(), Some("New Folder"));
    assert!(d.is_modified());
}

#[test]
fn export_import_and_copy_merged() {
    let mut f = Fixture::new("export", 4, 2);
    f.add(Spec { name: "Halves", size: (4.0, 2.0), pixels: Some(halves(4, 2, 2, RED, [0, 0, 255, 128])), ..Spec::default() });
    let mut d = doc(&f);
    let png = temp("export.png");
    d.export_png(&png).unwrap();
    let decoded = image::open(&png).unwrap().to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (4, 2));
    assert_eq!(decoded.get_pixel(0, 0).0, RED);
    assert_eq!(decoded.get_pixel(3, 1).0[3], 128);
    let jpg = temp("export.jpg");
    d.export_jpeg(&jpg, 0.9, [1.0, 1.0, 1.0]).unwrap();
    let decoded = image::open(&jpg).unwrap().to_rgb8();
    assert_eq!((decoded.width(), decoded.height()), (4, 2));
    let p = decoded.get_pixel(3, 1).0;
    assert!(p[2] > 200 && p[0] > 100, "half-transparent blue over white: {p:?}");
    assert!(d.jpeg_bytes(0.1, [0.0; 3]).unwrap().len() < d.jpeg_bytes(1.0, [0.0; 3]).unwrap().len());
    d.select_box(0.0, 0.0, 2.0, 2.0, false, Mode::Replace, false).unwrap();
    let (bytes, region) = d.copy_merged().unwrap().unwrap();
    assert_eq!(region, (0, 0, 2, 2));
    let copied = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!((copied.width(), copied.height()), (2, 2));
    assert_eq!(copied.get_pixel(1, 1).0, RED);
    let id = d.import_image(&png).unwrap();
    assert_eq!(d.renderer.image_size(id), Some((4, 2)));
    assert_eq!(d.renderer.layer(id).name, format!("compositor-p6-{}-export", std::process::id()));
    assert_eq!(d.undo_name(), Some("Import Image"));
    let id = d.import_image(&jpg).unwrap();
    assert_eq!(d.renderer.image_size(id), Some((4, 2)));
    std::fs::remove_file(&png).unwrap();
    std::fs::remove_file(&jpg).unwrap();
}

#[test]
fn canvas_size_anchors_fills_and_crops() {
    let mut f = Fixture::new("canvas", 2, 2);
    f.add(Spec { name: "Red", size: (2.0, 2.0), pixels: Some(solid(2, 2, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.canvas_size(4, 4, 4, None, None, "Canvas Size").unwrap();
    assert_eq!((d.width(), d.height()), (4, 4));
    let px = flat(&mut d);
    assert_pixel(&px, 4, 0, 0, CLEAR, 0);
    assert_pixel(&px, 4, 1, 1, RED, 0);
    assert_pixel(&px, 4, 2, 2, RED, 0);
    assert_pixel(&px, 4, 3, 3, CLEAR, 0);
    d.undo();
    assert_eq!((d.width(), d.height()), (2, 2));
    d.canvas_size(4, 4, 0, Some([0.0, 0.0, 1.0]), None, "Canvas Size").unwrap();
    let px = flat(&mut d);
    assert_pixel(&px, 4, 0, 0, RED, 0);
    assert_pixel(&px, 4, 3, 3, BLUE, 0);
    assert_eq!(d.renderer.layers()[0].name, "Canvas Extension");
    d.select_box(1.0, 1.0, 2.0, 2.0, false, Mode::Replace, false).unwrap();
    d.crop_to_selection().unwrap();
    assert_eq!((d.width(), d.height()), (2, 2));
    let px = flat(&mut d);
    assert_pixel(&px, 2, 0, 0, RED, 0);
    assert_pixel(&px, 2, 1, 1, BLUE, 0);
    assert!(d.selection.is_none());
}

#[test]
fn image_size_resamples_layers_and_masks() {
    let mut f = Fixture::new("image-size", 2, 2);
    let id = f.add(Spec { name: "Pair", size: (2.0, 2.0), sampling: "Nearest", pixels: Some(halves(2, 2, 1, RED, BLUE)), mask: Some((2, 2, vec![255, 255, 0, 0])), ..Spec::default() });
    let mut d = doc(&f);
    d.image_size(4, 4, 300.0, format::Sampling::Nearest).unwrap();
    assert_eq!((d.width(), d.height(), d.renderer.resolution()), (4, 4, 300.0));
    assert_eq!(d.renderer.image_size(id), Some((4, 4)));
    assert_eq!(d.renderer.mask(id).map(|m| (m.width(), m.height())), Some((4, 4)));
    let px = flat(&mut d);
    assert_pixel(&px, 4, 0, 0, RED, 0);
    assert_pixel(&px, 4, 3, 1, BLUE, 0);
    assert_pixel(&px, 4, 0, 3, CLEAR, 0);
    d.undo();
    assert_eq!(d.renderer.image_size(id), Some((2, 2)));
}

#[test]
fn selections_expand_and_contract() {
    let f = Fixture::new("expand", 9, 9);
    let mut d = doc(&f);
    d.select_box(4.0, 4.0, 1.0, 1.0, false, Mode::Replace, false).unwrap();
    d.resize_selection(2).unwrap();
    let s = d.selection.clone().unwrap();
    assert_eq!(s.bounds, Some((2, 2, 7, 7)));
    assert!(s.contains(4.0, 2.0) && !s.contains(2.0, 2.0), "round corners");
    d.resize_selection(-2).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((4, 4, 5, 5)));
    d.select_all().unwrap();
    d.resize_selection(-1).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((1, 1, 8, 8)), "contracting pulls away from the canvas edge");
}

#[test]
fn adjustment_layers_render_beneath_them() {
    let mut f = Fixture::new("adjust", 2, 1);
    let bottom = f.add(Spec { name: "Gray", size: (2.0, 1.0), pixels: Some(solid(2, 1, gray(192))), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(bottom);
    let id = d.add_adjustment("Levels").unwrap();
    assert_eq!(flat(&mut d)[0], gray(192), "identity levels change nothing");
    let mut levels = filters::Levels::default();
    levels.ranges[0].black = 128.0;
    d.set_adjustment(id, &Adjustment::Levels(levels.clone()), true);
    assert!(close(flat(&mut d)[0], gray(128), 2), "192 maps to about 128 with black at 128");
    d.renderer.set_opacity(id, 0.5);
    assert!(close(flat(&mut d)[0], gray(160), 2), "half opacity halves the effect");
    d.renderer.set_opacity(id, 1.0);
    d.select_box(0.0, 0.0, 1.0, 1.0, false, Mode::Replace, false).unwrap();
    d.add_mask(true).unwrap();
    let px = flat(&mut d);
    assert!(close(px[0], gray(192), 2) && close(px[1], gray(128), 2), "the layer's mask limits where it applies: {px:?}");
    d.undo();
    d.set_adjustment(id, &Adjustment::Exposure(filters::Exposure { exposure: 1.0, offset: 0.0, gamma: 1.0 }), true);
    assert!(flat(&mut d)[0][0] > 192, "a stop of exposure brightens");
    let mut hs = filters::HueSaturation::default();
    hs.colorize = true;
    hs.set_adjustment("Master", [0.0, 100.0, 0.0]);
    d.set_adjustment(id, &Adjustment::HueSaturation(hs), true);
    let p = flat(&mut d)[0];
    assert!(p[0] > 200 && p[1] < 150 && p[1] == p[2], "colorize at hue 0 makes a light gray a light red: {p:?}");
    let mut hs = filters::HueSaturation::default();
    hs.set_adjustment("Reds", [120.0, 0.0, 0.0]);
    d.set_adjustment(id, &Adjustment::HueSaturation(hs.clone()), true);
    assert!(close(flat(&mut d)[0], gray(192), 1), "a Reds shift leaves gray alone");
    // Round trip through the file record.
    let record = d.renderer.layer(id).adjustment.clone().unwrap();
    let Adjustment::HueSaturation(decoded) = Adjustment::from_record(&record).unwrap() else { panic!("kind") };
    assert_eq!(decoded.adjustments, hs.adjustments, "the ranges set come back");
    assert!(decoded.bands.len() == 7 && decoded.bands.iter().all(|(r, b)| *b == filters::default_band(r)), "bands are the defaults");
    // The record is in Swift's shape: dictionaries keyed by the ColorRange enum are flat key, value arrays.
    let adjustments = record.settings["hsvSettings"]["adjustments"].as_array().expect("flat array");
    assert_eq!(adjustments[0], serde_json::json!("Reds"));
    assert_eq!(adjustments[1]["hue"], serde_json::json!(120.0));
    assert_eq!(d.undo_name(), Some("Hue/Saturation Adjustment"));
}

#[test]
fn exposure_and_hue_saturation_run_as_filters() {
    let mut f = Fixture::new("hsv-filter", 1, 1);
    let id = f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let mut settings = filters::Settings::default();
    settings.hue_saturation.set_adjustment("Master", [120.0, 0.0, 0.0]);
    d.apply_filter(filters::Kind::HueSaturation, &settings).unwrap();
    let p = flat(&mut d)[0];
    assert!(p[1] > 200 && p[0] < 60, "red turned green: {p:?}");
    d.undo();
    let mut settings = filters::Settings::default();
    settings.exposure.exposure = -2.0;
    d.apply_filter(filters::Kind::Exposure, &settings).unwrap();
    assert!(flat(&mut d)[0][0] < 200);
}
