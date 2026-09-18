mod common;
use common::*;
use compositor::format::ProjectError;

#[test]
fn solid_layer_fills_canvas() {
    let mut f = Fixture::new("solid", 4, 4);
    f.add(Spec { name: "Red", size: (4.0, 4.0), pixels: Some(solid(4, 4, RED)), ..Spec::default() });
    let (px, warnings) = f.render();
    assert!(warnings.is_empty());
    for (i, p) in px.iter().enumerate() { assert!(close(*p, RED, 0), "pixel {i} is {p:?}"); }
}

#[test]
fn empty_canvas_is_transparent() {
    let f = Fixture::new("empty", 3, 2);
    let (px, _) = f.render();
    assert!(px.iter().all(|p| *p == CLEAR));
}

#[test]
fn multiply_blend() {
    let mut f = Fixture::new("multiply", 2, 2);
    f.add(Spec { name: "Red", size: (2.0, 2.0), pixels: Some(solid(2, 2, RED)), ..Spec::default() });
    f.add(Spec { name: "Gray", size: (2.0, 2.0), pixels: Some(solid(2, 2, [128, 128, 128, 255])), blend: Some("Multiply"), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, [128, 0, 0, 255], 1);
}

#[test]
fn screen_and_difference_blend() {
    let mut f = Fixture::new("screen", 1, 1);
    f.add(Spec { name: "Half", pixels: Some(solid(1, 1, [128, 128, 128, 255])), ..Spec::default() });
    f.add(Spec { name: "Screen", pixels: Some(solid(1, 1, [128, 128, 128, 255])), blend: Some("Screen"), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, [192, 192, 192, 255], 1); // 1 - (1 - a)(1 - b)
    let mut g = Fixture::new("difference", 1, 1);
    g.add(Spec { name: "White", pixels: Some(solid(1, 1, WHITE)), ..Spec::default() });
    g.add(Spec { name: "Diff", pixels: Some(solid(1, 1, [200, 50, 0, 255])), blend: Some("Difference"), ..Spec::default() });
    let (px, _) = g.render();
    assert_pixel(&px, 1, 0, 0, [55, 205, 255, 255], 1);
}

#[test]
fn opacity_half_over_black() {
    let mut f = Fixture::new("opacity", 1, 1);
    f.add(Spec { name: "Black", pixels: Some(solid(1, 1, BLACK)), ..Spec::default() });
    f.add(Spec { name: "White", pixels: Some(solid(1, 1, WHITE)), opacity: Some(0.5), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, [128, 128, 128, 255], 1);
}

#[test]
fn hidden_layer_and_hidden_folder() {
    let mut f = Fixture::new("hidden", 1, 1);
    f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    f.add(Spec { name: "Hidden blue", visible: false, pixels: Some(solid(1, 1, BLUE)), ..Spec::default() });
    let folder = f.add(Spec { name: "Hidden folder", visible: false, group: true, ..Spec::default() });
    f.add(Spec { name: "Inside", pixels: Some(solid(1, 1, WHITE)), parent: Some(folder), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, RED, 0);
}

#[test]
fn folder_contents_draw_in_array_order_under_the_folder() {
    // Layers: [red, folder, white-in-folder, blue] where blue sits after the folder at the root, so it is on top.
    let mut f = Fixture::new("order", 1, 1);
    f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    let folder = f.add(Spec { name: "Folder", group: true, ..Spec::default() });
    f.add(Spec { name: "Blue", pixels: Some(solid(1, 1, BLUE)), ..Spec::default() });
    f.add(Spec { name: "White", pixels: Some(solid(1, 1, WHITE)), parent: Some(folder), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, BLUE, 0);
}

#[test]
fn placement_flip_and_nearest_scale() {
    let two = (2, 1, vec![RED, BLUE]);
    let mut f = Fixture::new("placement", 2, 1);
    f.add(Spec { name: "Pair", size: (2.0, 1.0), pixels: Some(two.clone()), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, RED, 0);
    assert_pixel(&px, 2, 1, 0, BLUE, 0);

    let mut g = Fixture::new("flip", 2, 1);
    g.add(Spec { name: "Pair", size: (2.0, 1.0), flip: (true, false), pixels: Some(two.clone()), ..Spec::default() });
    let (px, _) = g.render();
    assert_pixel(&px, 2, 0, 0, BLUE, 0);
    assert_pixel(&px, 2, 1, 0, RED, 0);

    let mut s = Fixture::new("scale", 4, 2);
    s.add(Spec { name: "Pair", size: (4.0, 2.0), pixels: Some(two), ..Spec::default() });
    let (px, _) = s.render();
    for y in 0..2 { for x in 0..4 { assert_pixel(&px, 4, x, y, if x < 2 { RED } else { BLUE }, 0); } }

    let mut o = Fixture::new("offset", 4, 1);
    o.add(Spec { name: "Dot", origin: (2.0, 0.0), pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    let (px, _) = o.render();
    assert_pixel(&px, 4, 1, 0, CLEAR, 0);
    assert_pixel(&px, 4, 2, 0, RED, 0);
    assert_pixel(&px, 4, 3, 0, CLEAR, 0);
}

#[test]
fn rotate_90_is_clockwise() {
    // A red|blue pair centered at (1.5, 1) turned a quarter clockwise stands in column 1, red on top.
    let mut f = Fixture::new("rotate", 3, 2);
    f.add(Spec { name: "Pair", origin: (0.5, 0.5), size: (2.0, 1.0), rotation: 90.0, pixels: Some((2, 1, vec![RED, BLUE])), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 3, 1, 0, RED, 0);
    assert_pixel(&px, 3, 1, 1, BLUE, 0);
    assert_pixel(&px, 3, 0, 0, CLEAR, 0);
    assert_pixel(&px, 3, 2, 1, CLEAR, 0);
}

#[test]
fn layer_mask_hides_black_half() {
    let mut f = Fixture::new("mask", 2, 1);
    f.add(Spec { name: "Red", size: (2.0, 1.0), pixels: Some(solid(2, 1, RED)), mask: Some((2, 1, vec![255, 0])), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, RED, 0);
    assert_pixel(&px, 2, 1, 0, CLEAR, 0);
}

#[test]
fn disabled_mask_does_nothing() {
    let mut f = Fixture::new("mask-off", 2, 1);
    f.add(Spec { name: "Red", size: (2.0, 1.0), pixels: Some(solid(2, 1, RED)), mask: Some((2, 1, vec![255, 0])), mask_enabled: Some(false), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 1, 0, RED, 0);
}

#[test]
fn soft_mask_scales_alpha() {
    let mut f = Fixture::new("soft-mask", 1, 1);
    f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), mask: Some((1, 1, vec![128])), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, [255, 0, 0, 128], 1);
}

#[test]
fn folder_mask_clips_every_child() {
    let mut f = Fixture::new("folder-mask", 2, 1);
    let folder = f.add(Spec { name: "Folder", group: true, size: (2.0, 1.0), sampling: "High quality", mask: Some((2, 1, vec![255, 0])), ..Spec::default() });
    f.add(Spec { name: "Red", size: (2.0, 1.0), pixels: Some(solid(2, 1, RED)), parent: Some(folder), ..Spec::default() });
    let inner = f.add(Spec { name: "Inner", group: true, parent: Some(folder), ..Spec::default() });
    f.add(Spec { name: "Blue", size: (2.0, 1.0), pixels: Some(solid(2, 1, BLUE)), parent: Some(inner), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, BLUE, 0);
    assert_pixel(&px, 2, 1, 0, CLEAR, 0);
}

#[test]
fn clipping_stack_takes_base_alpha() {
    // Base: red on the left, transparent on the right. Clipped blue everywhere shows only on the left.
    let mut f = Fixture::new("clip", 2, 1);
    let base = f.add(Spec { name: "Base", size: (2.0, 1.0), pixels: Some(halves(2, 1, 1, RED, CLEAR)), ..Spec::default() });
    f.add(Spec { name: "Blue", size: (2.0, 1.0), pixels: Some(solid(2, 1, BLUE)), mask_source: Some(base), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, BLUE, 0);
    assert_pixel(&px, 2, 1, 0, CLEAR, 0);
}

#[test]
fn clipping_stack_blends_on_base_colors_without_thickening() {
    // A half-transparent red base with a 50% gray multiplied onto it: the color halves, the alpha stays 128.
    let mut f = Fixture::new("clip-blend", 1, 1);
    let base = f.add(Spec { name: "Base", pixels: Some(solid(1, 1, [255, 0, 0, 128])), ..Spec::default() });
    f.add(Spec { name: "Gray", pixels: Some(solid(1, 1, [128, 128, 128, 255])), blend: Some("Multiply"), mask_source: Some(base), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 1, 0, 0, [128, 0, 0, 128], 2);
}

#[test]
fn clipping_stack_composites_with_base_blend_mode() {
    let mut f = Fixture::new("clip-base-blend", 1, 1);
    f.add(Spec { name: "White", pixels: Some(solid(1, 1, WHITE)), ..Spec::default() });
    let base = f.add(Spec { name: "Base", pixels: Some(solid(1, 1, [128, 128, 128, 255])), blend: Some("Multiply"), ..Spec::default() });
    f.add(Spec { name: "Blue", pixels: Some(solid(1, 1, BLUE)), mask_source: Some(base), ..Spec::default() });
    let (px, _) = f.render();
    // Blue replaces the base's color inside the stack; the stack then multiplies onto white.
    assert_pixel(&px, 1, 0, 0, BLUE, 1);
}

#[test]
fn clip_to_a_non_adjacent_source_uses_its_coverage() {
    // Source sits at the bottom, an unrelated layer between, then the clipped layer: coverage still comes from the source.
    let mut f = Fixture::new("clip-far", 2, 1);
    let source = f.add(Spec { name: "Source", size: (2.0, 1.0), pixels: Some(halves(2, 1, 1, RED, CLEAR)), ..Spec::default() });
    f.add(Spec { name: "Between", size: (2.0, 1.0), pixels: Some(solid(2, 1, WHITE)), ..Spec::default() });
    f.add(Spec { name: "Blue", size: (2.0, 1.0), pixels: Some(solid(2, 1, BLUE)), mask_source: Some(source), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 2, 0, 0, BLUE, 0);
    assert_pixel(&px, 2, 1, 0, WHITE, 0);
}

#[test]
fn mask_placed_apart_from_its_layer() {
    // A 4x4 mask, white on top and black below, moved up by two rows: the black half now covers the top of the layer,
    // and the rows it left behind take the mask's background, which is white.
    let gray: Vec<u8> = (0..16).map(|i| if i < 8 { 255 } else { 0 }).collect();
    let mut f = Fixture::new("mask-placed", 4, 4);
    f.add(Spec { name: "Red", size: (4.0, 4.0), sampling: "High quality", pixels: Some(solid(4, 4, RED)), mask: Some((4, 4, gray)),
        mask_placement: Some(((0.0, -2.0), (4.0, 4.0))), ..Spec::default() });
    let (px, _) = f.render();
    assert_pixel(&px, 4, 1, 0, CLEAR, 0);
    assert_pixel(&px, 4, 1, 1, CLEAR, 0);
    assert_pixel(&px, 4, 1, 2, RED, 0);
    assert_pixel(&px, 4, 1, 3, RED, 0);
}

#[test]
fn downsampling_averages_a_checkerboard() {
    // A 32x32 one-pixel checker drawn at 8x8 goes through two sharp halvings. Interior pixels average to purple;
    // the outermost ring fades a little because, as in the reference, halvings read transparent past the edge.
    let checker: Vec<[u8; 4]> = (0..32 * 32).map(|i| if (i % 32 + i / 32) % 2 == 0 { RED } else { BLUE }).collect();
    let mut f = Fixture::new("downsample", 8, 8);
    f.add(Spec { name: "Checker", size: (8.0, 8.0), sampling: "High quality", pixels: Some((32, 32, checker)), ..Spec::default() });
    let (px, _) = f.render();
    for (x, y) in [(3, 3), (4, 4), (3, 4), (4, 3)] { assert_pixel(&px, 8, x, y, [128, 0, 128, 255], 6); }
    let corner = px[0];
    assert!(close(corner, [128, 0, 128, corner[3]], 8) && (180..=255).contains(&corner[3]), "corner is {corner:?}");
}

#[test]
fn identity_adjustment_layer_renders_without_change() {
    let mut f = Fixture::new("adjustment", 1, 1);
    f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    f.add(Spec { name: "Levels", adjustment: Some(serde_json::json!({"kind": "Levels"})), ..Spec::default() });
    let (px, warnings) = f.render();
    assert_pixel(&px, 1, 0, 0, RED, 0);
    assert!(warnings.is_empty());
}

#[test]
fn resolution_defaults_to_72() {
    let mut f = Fixture::new("dpi", 1, 1);
    f.extra.insert("resolution".into(), serde_json::json!(300));
    let project = f.load().unwrap();
    assert_eq!(project.manifest.resolution(), 300.0);
    f.extra.remove("resolution");
    assert_eq!(f.load().unwrap().manifest.resolution(), 72.0);
}

#[test]
fn rejects_bad_projects() {
    fn err(f: &Fixture) -> ProjectError { f.load().err().expect("should be rejected") }

    let mut f = Fixture::new("future", 1, 1);
    f.version = 8;
    assert_eq!(err(&f), ProjectError::Version(8));

    let mut f = Fixture::new("format", 1, 1);
    f.extra.insert("format".into(), serde_json::json!("com.example.other"));
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("huge", 40_000, 1);
    f.add(Spec::default());
    assert_eq!(err(&f), ProjectError::TooLarge);

    let mut f = Fixture::new("old-blend", 1, 1);
    f.version = 2;
    f.add(Spec { blend: Some("Multiply"), ..Spec::default() });
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("old-clip", 1, 1);
    f.version = 4;
    let a = f.add(Spec::default());
    f.add(Spec { mask_source: Some(a), ..Spec::default() });
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("group-image", 1, 1);
    f.add(Spec { group: true, pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("parent-not-group", 1, 1);
    let a = f.add(Spec::default());
    f.add(Spec { parent: Some(a), ..Spec::default() });
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("clip-cycle", 1, 1);
    let a = f.add(Spec::default());
    let b = f.add(Spec { mask_source: Some(a), ..Spec::default() });
    let object = f.layers[0].as_object_mut().unwrap();
    object.insert("maskSourceID".into(), serde_json::json!(b.hyphenated().to_string().to_uppercase()));
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("missing-image", 1, 1);
    f.add(Spec { pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    let dir = f.write();
    for entry in std::fs::read_dir(dir.join("images")).unwrap() { std::fs::remove_file(entry.unwrap().path()).unwrap(); }
    assert_eq!(err(&f), ProjectError::TooLarge);

    let mut f = Fixture::new("mask-rgba", 1, 1);
    let id = f.add(Spec { pixels: Some(solid(1, 1, RED)), mask: Some((1, 1, vec![255])), ..Spec::default() });
    let upper = id.hyphenated().to_string().to_uppercase();
    write_png(&f.dir.join("images").join(format!("{upper}.mask.png")), 1, 1, png::ColorType::Rgba, &[255, 255, 255, 255]);
    assert_eq!(err(&f), ProjectError::Invalid);

    let mut f = Fixture::new("blank-name", 1, 1);
    f.add(Spec { name: "   ", ..Spec::default() });
    assert_eq!(err(&f), ProjectError::Invalid);
}

#[test]
fn live_edits_change_the_next_draw() {
    use compositor::format::BlendMode;
    use compositor::raster::with_bytes;
    use compositor::render::Renderer;
    let mut f = Fixture::new("live", 1, 1);
    f.add(Spec { name: "Red", pixels: Some(solid(1, 1, RED)), ..Spec::default() });
    let top = f.add(Spec { name: "Gray", pixels: Some(solid(1, 1, [128, 128, 128, 255])), ..Spec::default() });
    let mut renderer = Renderer::new(f.load().unwrap()).unwrap();
    let pixel = |r: &mut Renderer| {
        let mut image = r.render_flat().unwrap();
        with_bytes(&mut image, |data, _| [data[2], data[1], data[0], data[3]]).unwrap()
    };
    assert_eq!(pixel(&mut renderer), [128, 128, 128, 255]);
    renderer.set_blend_mode(top, BlendMode::Multiply);
    assert!(close(pixel(&mut renderer), [128, 0, 0, 255], 1));
    renderer.set_opacity(top, 0.0);
    assert_eq!(pixel(&mut renderer), RED);
    renderer.set_opacity(top, 1.0);
    renderer.set_visible(top, false);
    assert_eq!(pixel(&mut renderer), RED);
    assert!(renderer.thumbnail(top, 36).unwrap().is_some());
}
