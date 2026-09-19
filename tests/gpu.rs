//! The GPU compositor against the CPU renderer: the same documents rendered both ways must agree, within
//! the rounding of 8-bit compositing and the different resampling filters at edges. Skipped without a GPU.

mod common;

use common::*;
use compositor::document::Document;
use compositor::gpu::Gpu;

fn cpu(d: &mut Document) -> Vec<u8> {
    let s = d.renderer.render_flat().unwrap();
    let (w, h) = (s.width() as usize, s.height() as usize);
    compositor::raster::with_bytes(&s, |b, stride| (0..h).flat_map(|y| b[y * stride..y * stride + w * 4].to_vec()).collect()).unwrap()
}

fn gpu(g: &mut Gpu, d: &mut Document) -> Option<Vec<u8>> {
    let (w, h) = (d.width() as u32, d.height() as u32);
    let plan = d.renderer.gpu_plan(cairo::Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0), w, h, g.max_dimension()).unwrap()?;
    Some(g.render(&plan).unwrap())
}

/// (largest difference, mean difference) over every byte.
fn compare(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let mut max = 0u8;
    let mut sum = 0u64;
    for (x, y) in a.iter().zip(b) { let d = x.abs_diff(*y); max = max.max(d); sum += d as u64; }
    (max, sum as f64 / a.len().max(1) as f64)
}

fn checker(w: u32, h: u32, a: [u8; 4], b: [u8; 4]) -> (u32, u32, Vec<[u8; 4]>) {
    (w, h, (0..w * h).map(|i| if ((i % w) / 4 + (i / w) / 4) % 2 == 0 { a } else { b }).collect())
}

#[test]
fn gpu_matches_cpu_on_blends_masks_folders_clips_and_adjustments() {
    let Some(mut g) = Gpu::new() else { eprintln!("no GPU; skipped"); return };
    eprintln!("GPU: {}", g.name);

    // Flat layers with every blend mode at partial opacity.
    for blend in ["Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "Difference", "Color Dodge", "Color Burn", "Hue", "Saturation", "Color", "Luminosity"] {
        let mut f = Fixture::new(&format!("gpu-{}", blend.replace(' ', "-")), 32, 32);
        f.add(Spec { name: "Base", size: (32.0, 32.0), pixels: Some(checker(32, 32, [200, 60, 30, 255], [40, 90, 220, 255])), ..Spec::default() });
        f.add(Spec { name: "Top", origin: (4.0, 6.0), size: (20.0, 16.0), pixels: Some(checker(20, 16, [30, 220, 90, 255], [250, 250, 40, 128])), opacity: Some(0.7), blend: Some(blend), ..Spec::default() });
        let mut d = Document::new(f.load().unwrap()).unwrap();
        let (c, gp) = (cpu(&mut d), gpu(&mut g, &mut d).expect("plan"));
        let (max, mean) = compare(&c, &gp);
        assert!(max <= 3 && mean < 0.5, "{blend}: max {max} mean {mean:.3}");
    }

    // A mask on the layer's grid, a folder mask, a clipping stack, a scaled nearest layer, a rotated smooth layer.
    let mut f = Fixture::new("gpu-structure", 64, 48);
    let folder = f.add(Spec { name: "Folder", group: true, size: (64.0, 48.0), mask: Some((64, 48, (0..64 * 48).map(|i| if (i % 64) < 40 { 255 } else { 60 }).collect())), ..Spec::default() });
    f.add(Spec { name: "Back", size: (64.0, 48.0), pixels: Some(checker(64, 48, [120, 120, 120, 255], [180, 180, 180, 255])), parent: Some(folder), ..Spec::default() });
    let base = f.add(Spec { name: "Masked", origin: (8.0, 8.0), size: (32.0, 24.0), pixels: Some(checker(32, 24, [255, 0, 0, 255], [255, 0, 0, 90])), mask: Some((32, 24, (0..32 * 24).map(|i| ((i % 32) * 8).min(255) as u8).collect())), parent: Some(folder), ..Spec::default() });
    f.add(Spec { name: "Clipped", origin: (0.0, 12.0), size: (64.0, 12.0), pixels: Some(checker(64, 12, [0, 0, 255, 255], [0, 255, 255, 200])), mask_source: Some(base), opacity: Some(0.8), blend: Some("Screen"), parent: Some(folder), ..Spec::default() });
    f.add(Spec { name: "Big pixels", origin: (40.0, 4.0), size: (20.0, 20.0), sampling: "Nearest", pixels: Some(checker(5, 5, [10, 200, 10, 255], [200, 10, 200, 255])), ..Spec::default() });
    f.add(Spec { name: "Turned", origin: (30.0, 20.0), size: (24.0, 16.0), rotation: 30.0, sampling: "High quality", pixels: Some(checker(24, 16, [255, 255, 255, 255], [0, 0, 0, 255])), opacity: Some(0.9), ..Spec::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    let (c, gp) = (cpu(&mut d), gpu(&mut g, &mut d).expect("plan"));
    let (max, mean) = compare(&c, &gp);
    assert!(mean < 2.0, "structure: max {max} mean {mean:.3}");
    // Away from the rotated layer's edges the pixels agree closely: probe a few.
    for (x, y) in [(2usize, 2usize), (20, 20), (12, 40), (50, 40)] {
        let i = (y * 64 + x) * 4;
        let dd = (0..4).map(|k| c[i + k].abs_diff(gp[i + k])).max().unwrap();
        assert!(dd <= 4, "at {x},{y}: cpu {:?} gpu {:?}", &c[i..i + 4], &gp[i..i + 4]);
    }

    // Adjustment layers: Levels through a mask, and a gradient map at half opacity.
    let mut d = Document::blank(40, 40, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 40.0, 40.0), [0.6, 0.3, 0.2], 0.0).unwrap();
    d.add_shape_layer(false, (10.0, 10.0, 20.0, 20.0), [0.2, 0.5, 0.9], 0.0).unwrap();
    let levels = d.add_adjustment("Levels").unwrap();
    let mut adj = d.adjustment(levels).unwrap();
    if let compositor::filters::Adjustment::Levels(l) = &mut adj { l.ranges[0].gamma = 0.5; l.ranges[1].white = 200.0; }
    d.set_adjustment(levels, &adj, true);
    let mask = compositor::raster::a8_filled(40, 40, 255).unwrap();
    compositor::raster::with_bytes_raw_mut(&mask, |b, stride| { for y in 0..40 { for x in 0..20 { b[y * stride + x] = 80; } } }).unwrap();
    d.renderer.set_mask(levels, Some(mask));
    let map = d.add_adjustment("Gradient Map").unwrap();
    d.set_opacity(map, 0.5);
    let (c, gp) = (cpu(&mut d), gpu(&mut g, &mut d).expect("plan"));
    let (max, mean) = compare(&c, &gp);
    assert!(max <= 4 && mean < 0.6, "adjustments: max {max} mean {mean:.3}");

    // Layer effects: a drop shadow and a color overlay, and a half-transparent layer under them.
    let mut d = Document::blank(60, 60, 72.0).unwrap();
    let id = d.add_shape_layer(false, (10.0, 10.0, 30.0, 30.0), [0.0, 0.0, 1.0], 0.0).unwrap();
    d.set_opacity(id, 0.6);
    let mut e = compositor::effects::Effects::default();
    e.drop_shadow = Some(compositor::effects::Shadow::drop_default());
    e.color_overlay = Some(compositor::effects::Overlay { color: [1.0, 0.0, 0.0], opacity: 0.5, enabled: true });
    d.set_effects(id, Some(&e)).unwrap();
    let (c, gp) = (cpu(&mut d), gpu(&mut g, &mut d).expect("plan"));
    let (max, mean) = compare(&c, &gp);
    assert!(max <= 4 && mean < 0.6, "effects: max {max} mean {mean:.3}");

    // A view at a quarter of the size: the GPU's mipmaps stand in for the CPU's halvings.
    let mut f = Fixture::new("gpu-reduced", 128, 128);
    f.add(Spec { name: "Fine", size: (128.0, 128.0), pixels: Some(checker(128, 128, [255, 255, 255, 255], [0, 0, 0, 255])), ..Spec::default() });
    let mut d = Document::new(f.load().unwrap()).unwrap();
    let plan = d.renderer.gpu_plan(cairo::Matrix::new(4.0, 0.0, 0.0, 4.0, 0.0, 0.0), 32, 32, g.max_dimension()).unwrap().unwrap();
    let small = g.render(&plan).unwrap();
    let mean_gray = small.chunks_exact(4).map(|p| p[1] as f64).sum::<f64>() / (32.0 * 32.0);
    assert!((mean_gray - 127.5).abs() < 20.0, "a fine checker averages to gray when reduced: {mean_gray}");

    // A Hue/Saturation adjustment has no GPU path yet: the plan says so and the CPU takes the frame.
    let mut d = Document::blank(10, 10, 72.0).unwrap();
    d.add_shape_layer(false, (0.0, 0.0, 10.0, 10.0), [1.0, 0.0, 0.0], 0.0).unwrap();
    d.add_adjustment("Hue/Saturation").unwrap();
    assert!(gpu(&mut g, &mut d).is_none());
}
