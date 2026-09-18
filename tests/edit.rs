mod common;
use common::*;
use compositor::document::{Document, WandSettings};
use compositor::filters::{self, Kind, Settings};
use compositor::raster::with_bytes;
use compositor::selection::{Mode, Selection};
use uuid::Uuid;

fn doc(f: &Fixture) -> Document { Document::new(f.load().unwrap()).unwrap() }

/// Straight-alpha RGBA of a layer's own pixels, row-major.
fn layer_pixels(d: &Document, id: Uuid) -> Vec<[u8; 4]> {
    let image = d.renderer.image(id).unwrap();
    let (w, h) = (image.width() as usize, image.height() as usize);
    with_bytes(image, |data, stride| (0..w * h).map(|i| {
        let p = &data[(i / w) * stride + (i % w) * 4..][..4];
        let a = p[3] as u32;
        let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
        [un(p[2]), un(p[1]), un(p[0]), p[3]]
    }).collect()).unwrap()
}

fn gray(v: u8) -> [u8; 4] { [v, v, v, 255] }

#[test]
fn wand_selects_contiguous_color_and_combines() {
    let mut f = Fixture::new("wand", 4, 1);
    let id = f.add(Spec { name: "Pair", size: (4.0, 1.0), pixels: Some((4, 1, vec![RED, RED, BLUE, BLUE])), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let settings = WandSettings { tolerance: 0, ..WandSettings::default() };
    d.wand(0.5, 0.5, &settings, Mode::Replace).unwrap();
    let sel = d.selection.clone().expect("selected");
    assert_eq!(sel.bounds, Some((0, 0, 2, 1)));
    assert!(sel.contains(1.0, 0.0) && !sel.contains(2.0, 0.0));
    assert_eq!(sel.outline.len(), 1);
    assert_eq!(sel.outline[0].len(), 4);
    d.wand(3.5, 0.5, &settings, Mode::Add).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((0, 0, 4, 1)));
    d.wand(0.5, 0.5, &settings, Mode::Subtract).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 0, 4, 1)));
    assert!(d.can_undo() && d.undo_name() == Some("Magic Wand"));
    assert!(d.undo());
    assert!(d.selection.as_ref().unwrap().contains(0.0, 0.0));
    assert!(d.redo());
    assert!(!d.selection.as_ref().unwrap().contains(0.0, 0.0));
    d.select_all().unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((0, 0, 4, 1)));
    d.invert_selection().unwrap();
    assert!(d.selection.as_ref().unwrap().is_empty());
    d.deselect();
    assert!(d.selection.is_none());
    // Subtracting from nothing changes nothing; a New click on a miss deselects.
    d.wand(0.5, 0.5, &settings, Mode::Subtract).unwrap();
    assert!(d.selection.is_none());
}

#[test]
fn wand_reads_the_active_layer_or_the_composite() {
    let mut f = Fixture::new("wand-sample", 4, 1);
    f.add(Spec { name: "Bottom", size: (4.0, 1.0), pixels: Some((4, 1, vec![RED, RED, RED, [0, 255, 0, 255]])), ..Spec::default() });
    let top = f.add(Spec { name: "Top", size: (2.0, 1.0), pixels: Some(solid(2, 1, BLUE)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(top);
    let this_layer = WandSettings { tolerance: 0, ..WandSettings::default() };
    d.wand(2.5, 0.5, &this_layer, Mode::Replace).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 0, 4, 1)), "the top layer alone is transparent there");
    let all = WandSettings { tolerance: 0, sample_all_layers: true, ..WandSettings::default() };
    d.wand(2.5, 0.5, &all, Mode::Replace).unwrap();
    assert_eq!(d.selection.as_ref().unwrap().bounds, Some((2, 0, 3, 1)), "the composite shows one red pixel there");
}

#[test]
fn levels_apply_with_undo_and_redo() {
    let mut f = Fixture::new("levels", 2, 1);
    let id = f.add(Spec { name: "Ramp", size: (2.0, 1.0), pixels: Some((2, 1, vec![gray(128), gray(192)])), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let original = d.renderer.image(id).unwrap().to_raw_none();
    let mut settings = Settings::default();
    settings.levels.ranges[0].black = 128.0;
    d.apply_filter(Kind::Levels, &settings).unwrap();
    let px = layer_pixels(&d, id);
    assert!(close(px[0], gray(0), 1) && close(px[1], gray(128), 2), "got {px:?}");
    assert_eq!(d.undo_name(), Some("Levels"));
    assert!(d.undo());
    assert_eq!(d.renderer.image(id).unwrap().to_raw_none(), original, "undo puts the very same surface back");
    assert!(d.redo());
    assert!(close(layer_pixels(&d, id)[0], gray(0), 1));
    assert!(!d.redo());
}

#[test]
fn noise_is_seeded_and_stays_inside_the_selection() {
    let mut f = Fixture::new("noise", 8, 8);
    let id = f.add(Spec { name: "Gray", size: (8.0, 8.0), pixels: Some(solid(8, 8, gray(128))), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let left = Selection::from_shape(8, 8, false, |cr| { cr.rectangle(0.0, 0.0, 4.0, 8.0); cr.fill()?; Ok(()) }).unwrap();
    assert_eq!(left.bounds, Some((0, 0, 4, 8)));
    d.set_selection(Some(left), "Rectangular Marquee");
    let settings = Settings { amount: 50.0, seed: 7, ..Settings::default() };
    d.apply_filter(Kind::AddNoise, &settings).unwrap();
    let first = layer_pixels(&d, id);
    let (mut changed, mut untouched) = (0, 0);
    for y in 0..8 { for x in 0..8 {
        let p = first[y * 8 + x];
        if x < 4 { if p != gray(128) { changed += 1; } } else { assert_eq!(p, gray(128)); untouched += 1; }
    } }
    assert!(changed > 16 && untouched == 32, "changed {changed}");
    d.undo();
    d.apply_filter(Kind::AddNoise, &settings).unwrap();
    assert_eq!(layer_pixels(&d, id), first, "the same seed gives the same grain");
    d.undo();
    d.apply_filter(Kind::AddNoise, &Settings { seed: 8, ..settings }).unwrap();
    assert_ne!(layer_pixels(&d, id), first);
}

#[test]
fn lens_zero_is_identity_and_gradient_map_maps_tones() {
    let mut f = Fixture::new("gradient", 3, 1);
    let id = f.add(Spec { name: "Ramp", size: (3.0, 1.0), pixels: Some((3, 1, vec![gray(0), gray(128), gray(255)])), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    d.apply_filter(Kind::LensCorrection, &Settings::default()).unwrap();
    assert!(!d.can_undo(), "a distortion of zero changes nothing, so nothing was recorded");
    let mut settings = Settings::default();
    settings.gradient.highlights = [1.0, 0.0, 0.0];
    d.apply_filter(Kind::GradientMap, &settings).unwrap();
    let px = layer_pixels(&d, id);
    assert!(close(px[0], BLACK, 1) && close(px[1], [128, 0, 0, 255], 2) && close(px[2], RED, 1), "got {px:?}");
    settings.gradient.reversed = true;
    d.undo();
    d.apply_filter(Kind::GradientMap, &settings).unwrap();
    let px = layer_pixels(&d, id);
    assert!(close(px[0], RED, 1) && close(px[2], BLACK, 1), "got {px:?}");
}

#[test]
fn grain_zero_is_identity_and_grain_changes_pixels() {
    let mut f = Fixture::new("grain", 8, 8);
    let id = f.add(Spec { name: "Gray", size: (8.0, 8.0), pixels: Some(solid(8, 8, gray(128))), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let mut settings = Settings::default();
    settings.grain.amount = 0.0;
    d.apply_filter(Kind::Grain, &settings).unwrap();
    assert!(!d.can_undo());
    settings.grain.amount = 80.0;
    settings.seed = 3;
    d.apply_filter(Kind::Grain, &settings).unwrap();
    let px = layer_pixels(&d, id);
    assert!(px.iter().any(|p| *p != gray(128)));
    assert!(px.iter().all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255), "grain changes brightness only");
}

#[test]
fn fill_and_heal_work_inside_the_selection() {
    let mut f = Fixture::new("fill", 12, 12);
    let mut pixels = vec![RED; 144];
    for y in 5..7 { for x in 5..7 { pixels[y * 12 + x] = BLUE; } }
    let id = f.add(Spec { name: "Spot", size: (12.0, 12.0), pixels: Some((12, 12, pixels)), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    assert!(d.apply_filter(Kind::ContentAwareFill, &Settings::default()).is_err(), "needs a selection");
    let spot = Selection::from_shape(12, 12, false, |cr| { cr.rectangle(5.0, 5.0, 2.0, 2.0); cr.fill()?; Ok(()) }).unwrap();
    d.set_selection(Some(spot), "Marquee");
    d.apply_filter(Kind::ContentAwareFill, &Settings::default()).unwrap();
    let px = layer_pixels(&d, id);
    for y in 5..7 { for x in 5..7 { assert!(close(px[y * 12 + x], RED, 8), "filled pixel is {:?}", px[y * 12 + x]); } }
    assert_eq!(px[0], RED);
    // Healing blends the spot toward its surround (the membrane fill starts from the spot's own pixels), so
    // the result is judged as "red with at most a trace of blue" in every mode, and only inside the selection.
    for mode in 0..3 {
        d.undo();
        d.apply_filter(Kind::SpotHeal, &Settings { heal_mode: mode, ..Settings::default() }).unwrap();
        let px = layer_pixels(&d, id);
        for y in 5..7 { for x in 5..7 { let p = px[y * 12 + x]; assert!(p[0] > 200 && p[2] < 64 && p[3] == 255, "mode {mode} healed pixel is {p:?}"); } }
        assert_eq!(px[143], RED);
        assert_eq!(px[4 * 12 + 4], RED);
    }
}

#[test]
fn histogram_counts_the_selected_pixels() {
    let mut f = Fixture::new("histogram", 2, 1);
    let id = f.add(Spec { name: "Two", size: (2.0, 1.0), pixels: Some((2, 1, vec![gray(128), WHITE])), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let bins = d.histogram().unwrap();
    assert_eq!(bins[0][128], 1.0);
    assert_eq!(bins[0][255], 1.0);
    assert_eq!(bins[1][255], 1.0);
    let left = Selection::from_shape(2, 1, false, |cr| { cr.rectangle(0.0, 0.0, 1.0, 1.0); cr.fill()?; Ok(()) }).unwrap();
    d.set_selection(Some(left), "Marquee");
    let selected = d.histogram().unwrap();
    assert_eq!(selected[0][128], 1.0);
    assert_eq!(selected[0][255], 0.0);
    let auto = filters::Levels::auto_contrast(&bins);
    assert_eq!((auto.ranges[0].black, auto.ranges[0].white), (128.0, 255.0));
    assert_eq!(filters::Levels::auto_contrast(&selected), filters::Levels::default(), "one tone has nothing to stretch");
}

#[test]
fn selection_coverage_lands_on_the_layer_grid() {
    let mut f = Fixture::new("coverage", 4, 1);
    let id = f.add(Spec { name: "Pair", origin: (2.0, 0.0), size: (2.0, 1.0), pixels: Some(solid(2, 1, RED)), ..Spec::default() });
    let d = doc(&f);
    let sel = Selection::from_shape(4, 1, false, |cr| { cr.rectangle(2.0, 0.0, 1.0, 1.0); cr.fill()?; Ok(()) }).unwrap();
    let coverage = sel.coverage_on_layer(&d.renderer.layer(id).transform, 2, 1).unwrap();
    let bytes = with_bytes(&coverage, |data, _| [data[0], data[1]]).unwrap();
    assert_eq!(bytes, [255, 0]);
    let moved = sel.translated(1, 0).unwrap();
    assert_eq!(moved.bounds, Some((3, 0, 4, 1)));
}

#[test]
fn preview_shows_without_committing() {
    let mut f = Fixture::new("preview", 1, 1);
    let id = f.add(Spec { name: "Gray", pixels: Some(solid(1, 1, gray(128))), ..Spec::default() });
    let mut d = doc(&f);
    d.active = Some(id);
    let mut settings = Settings::default();
    settings.gradient.highlights = [1.0, 0.0, 0.0];
    d.preview_filter(Kind::GradientMap, &settings).unwrap();
    let mut shown = d.renderer.render_flat().unwrap();
    let p = with_bytes(&shown, |data, _| [data[2], data[1], data[0]]).unwrap();
    assert!(close([p[0], p[1], p[2], 255], [128, 0, 0, 255], 2), "preview drawn: {p:?}");
    assert_eq!(layer_pixels(&d, id)[0], gray(128), "pixels untouched");
    assert!(!d.can_undo());
    d.clear_preview();
    shown = d.renderer.render_flat().unwrap();
    let p = with_bytes(&shown, |data, _| data[1]).unwrap();
    assert_eq!(p, 128);
}
