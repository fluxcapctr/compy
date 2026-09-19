//! Type layers: text set with Pango on a layer that keeps its style for editing again.


use compositor::document::Document;
use compositor::text::TextStyle;

fn alpha_sum(d: &mut Document) -> u64 {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| b[(i / w) * stride + (i % w) * 4 + 3] as u64).sum()).unwrap()
}

#[test]
fn type_layers_render_edit_and_round_trip() {
    let mut d = Document::blank(200, 100, 72.0).unwrap();
    let style = TextStyle { text: "Hi there\nsecond".into(), size: 24.0, color: [1.0, 0.0, 0.0], ..TextStyle::default() };
    let id = d.add_text_layer(&style, 10.0, 12.0).unwrap();
    let layer = d.renderer.layer(id).clone();
    assert_eq!(layer.name, "Hi there", "named after the first line");
    assert_eq!((layer.transform.origin.0, layer.transform.origin.1), (10.0, 12.0));
    assert!(layer.transform.size.0 > 40.0 && layer.transform.size.1 > 40.0, "two lines of 24 px text: {:?}", layer.transform.size);
    assert_eq!(d.text_style(id).unwrap(), style);
    assert_eq!(d.undo_name(), Some("Type"));
    let ink = alpha_sum(&mut d);
    assert!(ink > 1000, "the text drew: {ink}");
    let (px, w) = { let s = d.renderer.render_flat().unwrap(); let w = s.width() as usize; (compositor::raster::with_bytes(&s, |b, stride| (0..w * 100).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; [p[2], p[1], p[0], p[3]] }).collect::<Vec<_>>()).unwrap(), w as u32) };
    let red = px.iter().filter(|p| p[3] > 200 && p[0] > 200 && p[1] < 30).count();
    assert!(red > 50, "the text is red: {red} of {}", w);
    assert_eq!(d.text_layer_at((12.0, 14.0)), Some(id));
    assert_eq!(d.text_layer_at((190.0, 90.0)), None);

    // Editing keeps the top-left corner, resizes to the new text, and merges into one undo step.
    let mut edited = style.clone();
    edited.text = "Hi".into();
    d.set_text(id, &edited).unwrap();
    edited.text = "Hi!".into();
    d.set_text(id, &edited).unwrap();
    let layer = d.renderer.layer(id).clone();
    assert_eq!(layer.name, "Hi!");
    assert_eq!((layer.transform.origin.0, layer.transform.origin.1), (10.0, 12.0));
    assert!(layer.transform.size.1 < 40.0, "one line now: {:?}", layer.transform.size);
    assert_eq!(d.undo_name(), Some("Edit Text"));
    d.undo();
    assert_eq!(d.text_style(id).unwrap().text, "Hi there\nsecond", "both edits undo together");
    d.redo();

    // A scaled type layer keeps its scale when the text changes.
    let mut t = d.renderer.layer(id).transform;
    let raster = d.renderer.image_size(id).unwrap();
    t.size = compositor::format::Size(raster.0 as f64 * 2.0, raster.1 as f64 * 2.0);
    d.set_transform(id, t, "Scale Layer");
    assert_eq!(d.renderer.image_size(id), Some(raster), "type layers scale rather than redraw");
    edited.text = "Hi!!".into();
    d.set_text(id, &edited).unwrap();
    let raster2 = d.renderer.image_size(id).unwrap();
    let size = d.renderer.layer(id).transform.size;
    assert!((size.0 - raster2.0 as f64 * 2.0).abs() < 0.01 && (size.1 - raster2.1 as f64 * 2.0).abs() < 0.01, "still twice its raster: {size:?} vs {raster2:?}");

    // Empty text is refused; a non-type layer is refused.
    edited.text = "   ".into();
    assert!(d.set_text(id, &edited).is_err());
    let base = d.renderer.layers()[0].id;
    assert!(d.set_text(base, &style).is_err());

    // The style survives save and open, and painting turns the layer into plain pixels.
    let path = std::env::temp_dir().join(format!("compositor-test-{}-type.comp", std::process::id()));
    d.save(&path).unwrap();
    let reopened = Document::new(compositor::format::load(&path).unwrap()).unwrap();
    assert_eq!(reopened.text_style(id).unwrap().text, "Hi!!");
    d.fill([0.0, 1.0, 0.0]).unwrap();
    assert!(d.text_style(id).is_none(), "filled pixels are no longer text");
}

#[test]
fn text_families_list_and_google_fetch_validates_the_name() {
    let families = compositor::text::families();
    assert!(!families.is_empty());
    assert!(compositor::text::fetch_google_family("   ").is_err());
}

/// Needs the network: fetches a small family from Google Fonts into the user's fonts.
#[test]
#[ignore]
fn google_font_fetch_installs_a_family() {
    let n = compositor::text::fetch_google_family("Lobster").unwrap();
    assert!(n >= 1, "fetched {n} files");
    assert!(compositor::text::families().iter().any(|f| f == "Lobster"));
}

fn pixels(d: &mut Document) -> (Vec<[u8; 4]>, u32) {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    let px = compositor::raster::with_bytes(&s, |b, stride| (0..w * h).map(|i| { let p = &b[(i / w) * stride + (i % w) * 4..][..4]; [p[2], p[1], p[0], p[3]] }).collect()).unwrap();
    (px, w as u32)
}

#[test]
fn layer_effects_render_around_the_layer_and_survive_saving() {
    use compositor::effects::{Effects, Shadow, Stroke, Overlay};
    let mut d = Document::blank(60, 60, 72.0).unwrap();
    let id = d.add_shape_layer(false, (20.0, 20.0, 20.0, 20.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    assert!(d.effects(id).is_none());
    let (px, w) = pixels(&mut d);
    assert_eq!(px[(45 * w + 45) as usize][3], 0, "nothing outside the square yet");

    // A hard drop shadow lower right, knocked out under the square.
    let mut e = Effects::default();
    e.drop_shadow = Some(Shadow { size: 0.0, distance: 6.0, opacity: 1.0, ..Shadow::drop_default() });
    d.set_effects(id, Some(&e)).unwrap();
    assert_eq!(d.undo_name(), Some("Layer Style"));
    let (px, w) = pixels(&mut d);
    let at = |x: u32, y: u32| px[(y * w + x) as usize];
    assert_eq!(at(30, 30), [0, 0, 255, 255], "the square is untouched");
    assert!(at(42, 43)[3] > 200 && at(42, 43)[0] < 20 && at(42, 43)[2] < 20, "black shadow lower right: {:?}", at(42, 43));
    assert_eq!(at(15, 15)[3], 0, "no shadow upper left");

    // A stroke outside and a color overlay, merged into the same undo step.
    e.stroke = Some(Stroke { size: 2.0, position: 0, color: [1.0, 0.0, 0.0], opacity: 1.0, enabled: true });
    e.color_overlay = Some(Overlay { color: [0.0, 1.0, 0.0], opacity: 1.0, enabled: true });
    d.set_effects(id, Some(&e)).unwrap();
    let (px, w) = pixels(&mut d);
    let at = |x: u32, y: u32| px[(y * w + x) as usize];
    assert_eq!(at(30, 30), [0, 255, 0, 255], "overlay recolors the square");
    assert_eq!(at(18, 30), [255, 0, 0, 255], "red stroke two pixels outside");
    assert_eq!(at(17, 30)[3], 0, "nothing three pixels out");
    d.undo();
    assert!(d.effects(id).is_none(), "all changes undo as one step");
    d.redo();
    assert_eq!(d.effects(id), Some(e.clone()));

    // Half opacity applies to the whole styled layer, and the record survives a save.
    d.set_opacity(id, 0.5);
    let (px, w) = pixels(&mut d);
    assert!((px[(30 * w + 30) as usize][3] as i32 - 128).abs() <= 2, "{:?}", px[(30 * w + 30) as usize]);
    let path = std::env::temp_dir().join(format!("compositor-test-{}-effects.comp", std::process::id()));
    d.save(&path).unwrap();
    let reopened = Document::new(compositor::format::load(&path).unwrap()).unwrap();
    assert_eq!(reopened.effects(id), Some(e));
    // Effects are for pixel layers; clearing takes the record off.
    let base = d.renderer.layers()[0].id;
    d.set_effects(id, None).unwrap();
    assert!(d.effects(id).is_none());
    let mut folder = Document::blank(10, 10, 72.0).unwrap();
    let f = folder.add_folder();
    assert!(folder.set_effects(f, Some(&Effects { stroke: Some(Stroke::default()), ..Effects::default() })).is_err());
    let _ = base;
}
