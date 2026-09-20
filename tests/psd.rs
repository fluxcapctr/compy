//! Photoshop files: a round trip through the app's own writer, and a file written by ImageMagick.

mod common;

use common::*;
use compositor::document::Document;
use compositor::psd;
use compositor::raster::with_bytes;
use std::path::PathBuf;
use std::process::Command;

fn temp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("compositor-test-{}-{name}", std::process::id())) }

/// Straight RGBA pixels of a rendered document.
fn flat(document: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let image = document.renderer.render_flat().unwrap();
    let (w, h) = (image.width() as usize, image.height() as usize);
    let pixels = with_bytes(&image, |data, stride| {
        (0..w * h).map(|i| {
            let p = &data[(i / w) * stride + (i % w) * 4..][..4];
            let a = p[3] as u32;
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            [un(p[2]), un(p[1]), un(p[0]), p[3]]
        }).collect()
    }).unwrap();
    (pixels, w as u32)
}

#[test]
fn round_trip_keeps_layers_folders_masks_and_blends() {
    let mut fixture = Fixture::new("psd-rt", 40, 30);
    fixture.add(Spec { name: "Back", size: (40.0, 30.0), pixels: Some(solid(40, 30, WHITE)), ..Default::default() });
    let folder = fixture.add(Spec { name: "Folder", group: true, size: (40.0, 30.0), ..Default::default() });
    fixture.add(Spec { name: "Red", origin: (10.0, 5.0), size: (20.0, 20.0), pixels: Some(solid(20, 20, RED)), parent: Some(folder), opacity: Some(0.5), blend: Some("Multiply"), ..Default::default() });
    // Blue over the right half only, via a mask with the left half black.
    fixture.add(Spec { name: "Blue", size: (40.0, 30.0), pixels: Some(solid(40, 30, BLUE)), mask: Some((40, 30, (0..40 * 30).map(|i| if i % 40 < 20 { 0 } else { 255 }).collect())), ..Default::default() });
    let mut document = Document::new(fixture.load().unwrap()).unwrap();
    let (before, _) = flat(&mut document);
    let path = temp("rt.psd");
    let warnings = document.export_psd(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");

    let (project, warnings) = psd::read(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let layers = &project.manifest.layers;
    assert_eq!(layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Back", "Red", "Folder", "Blue"]);
    let red = layers.iter().find(|l| l.name == "Red").unwrap();
    let folder_id = layers.iter().find(|l| l.name == "Folder").unwrap().id;
    assert_eq!(red.parent_id, Some(folder_id));
    assert_eq!(red.transform.origin.0, 10.0);
    assert_eq!(red.transform.size.1, 20.0);
    assert!((red.opacity() - 0.5).abs() < 0.01);
    assert_eq!(red.blend_mode(), compositor::format::BlendMode::Multiply);
    assert!(layers.iter().find(|l| l.name == "Blue").unwrap().mask_file.is_some());
    assert_eq!(layers.iter().find(|l| l.name == "Folder").unwrap().is_group(), true);

    let mut reopened = Document::new(project).unwrap();
    let (after, w) = flat(&mut reopened);
    assert_eq!(before.len(), after.len());
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(close(*a, *b, 2), "pixel {},{}: {a:?} vs {b:?}", i as u32 % w, i as u32 / w);
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_is_readable_by_imagemagick() {
    let mut fixture = Fixture::new("psd-im", 24, 16);
    fixture.add(Spec { name: "A", size: (24.0, 16.0), pixels: Some(halves(24, 16, 12, RED, BLUE)), ..Default::default() });
    fixture.add(Spec { name: "B", origin: (4.0, 4.0), size: (8.0, 8.0), pixels: Some(solid(8, 8, WHITE)), ..Default::default() });
    let mut document = Document::new(fixture.load().unwrap()).unwrap();
    let path = temp("im-read.psd");
    document.export_psd(&path).unwrap();
    let Ok(output) = Command::new("magick").args(["identify", "-format", "%w %h %[label]\n"]).arg(&path).output() else { return };
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    // The merged image, then each layer.
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("24 16"), "{text}");
    assert!(lines[1].starts_with("24 16 A"), "{text}");
    assert!(lines[2].starts_with("8 8 B"), "{text}");
    let flat = temp("im-flat.png");
    assert!(Command::new("magick").arg(format!("{}[0]", path.display())).arg(&flat).status().unwrap().success());
    // Read the flattened PNG through ImageMagick's own text output to keep this independent of the app.
    let dump = Command::new("magick").arg(&flat).args(["-depth", "8", "rgba:-"]).output().unwrap().stdout;
    let pixels: Vec<[u8; 4]> = dump.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
    assert_pixel(&pixels, 24, 1, 1, RED, 1);
    assert_pixel(&pixels, 24, 20, 8, BLUE, 1);
    assert_pixel(&pixels, 24, 6, 6, WHITE, 1);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&flat);
}

#[test]
fn opens_a_psd_written_by_imagemagick() {
    let path = temp("im-write.psd");
    // ImageMagick writes its first image as the merged preview and the rest as layers.
    let status = Command::new("magick").args(["-size", "40x30", "xc:#0000ff", "(", "-size", "40x30", "xc:#0000ff", "-set", "label", "Back", ")", "(", "-size", "20x20", "xc:#ff0000", "-set", "page", "+5+5", "-set", "label", "Top", ")", "-background", "none"]).arg(&path).status();
    let Ok(status) = status else { return };
    assert!(status.success());
    let (project, warnings) = psd::read(&path).unwrap();
    assert!(warnings.iter().all(|w| w.contains("16-bit")), "{warnings:?}");
    assert_eq!(project.manifest.width, 40);
    let layers = &project.manifest.layers;
    assert_eq!(layers.len(), 2, "{:?}", layers.iter().map(|l| &l.name).collect::<Vec<_>>());
    assert_eq!(layers[1].name, "Top");
    assert_eq!((layers[1].transform.origin.0, layers[1].transform.origin.1), (5.0, 5.0));
    let mut document = Document::new(project).unwrap();
    let (pixels, w) = flat(&mut document);
    assert_pixel(&pixels, w, 1, 1, BLUE, 1);
    assert_pixel(&pixels, w, 10, 10, RED, 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn opens_images_as_documents() {
    for (ext, color, expected) in [("png", "#ff0000", RED), ("jpg", "#0000ff", BLUE), ("gif", "#ffffff", WHITE), ("webp", "#ff0000", RED), ("bmp", "#0000ff", BLUE)] {
        let path = temp(&format!("open.{ext}"));
        let Ok(status) = Command::new("magick").args(["-size", "12x8", &format!("xc:{color}")]).arg(&path).status() else { return };
        assert!(status.success(), "{ext}");
        let mut document = Document::open_image(&path).unwrap_or_else(|e| panic!("{ext}: {e:#}"));
        assert_eq!((document.width(), document.height()), (12, 8), "{ext}");
        assert_eq!(document.renderer.layers().len(), 1);
        assert!(document.renderer.layers()[0].name.ends_with("-open"), "{ext}");
        assert!(!document.is_modified());
        let (pixels, w) = flat(&mut document);
        assert_pixel(&pixels, w, 3, 3, expected, 3);
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn refuses_what_it_cannot_read() {
    let path = temp("cmyk.psd");
    let Ok(status) = Command::new("magick").args(["-size", "8x8", "xc:red", "-colorspace", "CMYK"]).arg(&path).status() else { return };
    assert!(status.success());
    let message = match psd::read(&path) { Err(e) => e.to_string(), Ok(_) => panic!("CMYK opened") };
    assert!(message.contains("CMYK"), "{message}");
    let not_psd = temp("not.psd");
    std::fs::write(&not_psd, b"hello").unwrap();
    let message = match psd::read(&not_psd) { Err(e) => e.to_string(), Ok(_) => panic!("text opened") };
    assert!(message.contains("not a Photoshop"), "{message}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&not_psd);
}

#[test]
fn layer_styles_survive_a_psd_round_trip_and_type_is_flagged() {
    use compositor::effects::{Effects, Shadow, Stroke};
    let mut fixture = Fixture::new("psd-fx", 40, 30);
    fixture.add(Spec { name: "Back", size: (40.0, 30.0), pixels: Some(solid(40, 30, WHITE)), ..Default::default() });
    fixture.add(Spec { name: "Red", origin: (10.0, 5.0), size: (20.0, 20.0), pixels: Some(solid(20, 20, RED)), ..Default::default() });
    let mut document = Document::new(fixture.load().unwrap()).unwrap();
    let red = document.renderer.layers().iter().find(|l| l.name == "Red").unwrap().id;
    let mut e = Effects::default();
    e.drop_shadow = Some(Shadow { enabled: true, color: [0.0, 0.0, 0.0], opacity: 0.5, angle: 120.0, distance: 3.0, size: 4.0 });
    e.stroke = Some(Stroke { enabled: true, size: 2.0, position: 0, color: [0.0, 0.0, 1.0], opacity: 1.0 });
    document.set_effects(red, Some(&e)).unwrap();
    let style = compositor::text::TextStyle { text: "Hi".into(), size: 12.0, ..compositor::text::TextStyle::default() };
    document.add_text_layer(&style, 2.0, 2.0).unwrap();
    let path = temp("fx.psd");
    let warnings = document.export_psd(&path).unwrap();
    assert!(warnings.iter().any(|w| w.contains("written as pixels")), "{warnings:?}");
    let (project, warnings) = psd::read(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let back = project.manifest.layers.iter().find(|l| l.name == "Red").unwrap();
    let effects = Effects::from_record(back.effects.as_ref().expect("the style came back")).unwrap();
    let shadow = effects.drop_shadow.unwrap();
    assert!(shadow.enabled && (shadow.opacity - 0.5).abs() < 1e-9 && shadow.distance == 3.0 && shadow.size == 4.0);
    let stroke = effects.stroke.unwrap();
    assert!(stroke.size == 2.0 && stroke.position == 0 && stroke.color == [0.0, 0.0, 1.0]);
    // Reopened, the style renders: the stroke's blue sits just outside the red square.
    let mut reopened = Document::new(project).unwrap();
    let (px, w) = flat(&mut reopened);
    assert_pixel(&px, w, 9, 15, [0, 0, 255, 255], 8);
    let _ = std::fs::remove_file(&path);
}
