//! Export Sizes: one document written out at several sizes at once, each a copy scaled and cropped or
//! padded to the target, or reframed so the photo fills the frame while the smaller elements (type,
//! logos, shapes) keep their relative places and stay inside a safe margin.

use crate::document::Document;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// A named target size.
#[derive(Clone, Debug, PartialEq)]
pub struct SizePreset { pub name: String, pub width: i32, pub height: i32 }

/// How the document meets a frame of another shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Scale to cover the frame and crop the overflow, centered.
    Fill,
    /// Scale to fit inside the frame and pad the rest with a color.
    Pad,
    /// Fill with the photo, then place the smaller elements where they sat, scaled to the frame's short side.
    Reframe,
}

impl Fit {
    pub fn from_name(name: &str) -> Option<Fit> { match name.trim().to_lowercase().as_str() { "fill" | "cover" | "crop" => Some(Fit::Fill), "pad" | "fit" | "letterbox" => Some(Fit::Pad), "reframe" | "smart" => Some(Fit::Reframe), _ => None } }
    pub fn name(self) -> &'static str { match self { Fit::Fill => "Fill", Fit::Pad => "Pad", Fit::Reframe => "Reframe" } }
}

/// The sizes the dialog offers, with the pixels they mean.
pub fn presets() -> Vec<SizePreset> {
    let p = |name: &str, width: i32, height: i32| SizePreset { name: name.into(), width, height };
    vec![
        p("Instagram post", 1080, 1080), p("Instagram portrait", 1080, 1350), p("Story or Reel", 1080, 1920),
        p("Facebook post", 1200, 630), p("X post", 1600, 900), p("LinkedIn post", 1200, 627), p("YouTube thumbnail", 1280, 720), p("Pinterest pin", 1000, 1500),
        p("Leaderboard", 728, 90), p("Medium rectangle", 300, 250), p("Wide skyscraper", 160, 600), p("Billboard", 970, 250), p("Half page", 300, 600),
        p("HD", 1920, 1080), p("4K", 3840, 2160), p("Square 2048", 2048, 2048),
        p("Letter 300 ppi", 2550, 3300), p("A4 300 ppi", 2480, 3508), p("Tabloid 300 ppi", 3300, 5100),
    ]
}

/// A preset by name, loosely ("story", "instagram post", "a4").
pub fn preset_named(name: &str) -> Option<SizePreset> {
    let want = name.trim().to_lowercase();
    let all = presets();
    all.iter().find(|p| p.name.to_lowercase() == want).or_else(|| all.iter().find(|p| p.name.to_lowercase().contains(&want))).cloned()
}

/// What to write: PNG, or JPEG at a quality (0 to 1) over a background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format { Png, Jpeg(f64) }

/// A file name for one size: "poster-Story or Reel-1080x1920.png".
pub fn file_name(title: &str, preset: &SizePreset, format: Format) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect::<String>().trim().to_string();
    let ext = match format { Format::Png => "png", Format::Jpeg(_) => "jpg" };
    format!("{}-{}-{}x{}.{ext}", clean(title), clean(&preset.name), preset.width, preset.height)
}

/// A copy of `document` remade for `preset` in `fit`, ready to render.
pub fn remake(document: &Document, preset: &SizePreset, fit: Fit, background: [f64; 3]) -> Result<Document> {
    let (w, h) = (preset.width, preset.height);
    if w < 1 || h < 1 || w > 30_000 || h > 30_000 || w as i64 * h as i64 > 100_000_000 { bail!("{} x {} is not a size that can be made", w, h); }
    let (dw, dh) = (document.width() as f64, document.height() as f64);
    let (rw, rh) = (w as f64 / dw, h as f64 / dh);
    let scale = match fit { Fit::Pad => rw.min(rh), Fit::Fill | Fit::Reframe => rw.max(rh) };
    let mut copy = document.duplicate()?;
    let elements: Vec<(uuid::Uuid, (f64, f64, f64, f64))> = if fit == Fit::Reframe { element_layers(document) } else { Vec::new() };
    let (sw, sh) = (((dw * scale).round() as i32).max(1), ((dh * scale).round() as i32).max(1));
    if (sw, sh) != (dw as i32, dh as i32) { copy.image_size(sw, sh, copy.renderer.resolution(), crate::format::Sampling::High)?; }
    let fill = if fit == Fit::Pad { Some(background) } else { None };
    // The scaled copy centered in the frame: cropped (fill) or padded (pad).
    let offset = (((w - sw) as f64 / 2.0).floor(), ((h - sh) as f64 / 2.0).floor());
    copy.canvas_size(w, h, 4, fill, Some(offset), "Export Size")?;
    if fit == Fit::Reframe {
        // The elements go back where they sat, in the frame's proportions, at the short side's scale, and
        // inside a five percent margin.
        let element_scale = rw.min(rh) / scale;
        let margin = (w.min(h) as f64 * 0.05).round();
        for (id, (bx0, by0, bx1, by1)) in elements {
            if !copy.has_layer(id) { continue; }
            let t = copy.renderer.layer(id).transform;
            let (cx, cy) = ((bx0 + bx1) / 2.0 / dw, (by0 + by1) / 2.0 / dh);
            let mut size = crate::format::Size((t.size.0 * element_scale).max(1.0), (t.size.1 * element_scale).max(1.0));
            // An element wider than the frame's inside shrinks to it.
            let inside = (w as f64 - 2.0 * margin, h as f64 - 2.0 * margin);
            let shrink = (inside.0 / size.0).min(inside.1 / size.1).min(1.0);
            if shrink < 1.0 { size = crate::format::Size(size.0 * shrink, size.1 * shrink); }
            let mut origin = crate::format::Point((cx * w as f64 - size.0 / 2.0).round(), (cy * h as f64 - size.1 / 2.0).round());
            origin.0 = origin.0.clamp(margin, (w as f64 - margin - size.0).max(margin));
            origin.1 = origin.1.clamp(margin, (h as f64 - margin - size.1).max(margin));
            let mut moved = t;
            moved.origin = origin;
            moved.size = size;
            copy.set_transform(id, moved, "Reframe");
        }
    }
    Ok(copy)
}

/// The layers that are elements rather than the picture: type, and anything covering less than half the
/// canvas; with their bounds in the original document.
fn element_layers(document: &Document) -> Vec<(uuid::Uuid, (f64, f64, f64, f64))> {
    let area = document.width() as f64 * document.height() as f64;
    document.renderer.layers().iter().filter(|l| !l.is_group() && l.adjustment.is_none()).filter_map(|l| {
        let b = l.transform.bounds();
        let covers = ((b.2 - b.0) * (b.3 - b.1)) / area;
        // Type is always an element; anything else is the picture once it covers half the canvas (a
        // background rectangle counts as the picture, a small shape as an element).
        if l.text.is_some() || covers < 0.5 { Some((l.id, b)) } else { None }
    }).collect()
}

/// Writes every size into `folder`, returning the files made and any size that failed.
pub fn export_all(document: &Document, title: &str, sizes: &[SizePreset], fit: Fit, format: Format, background: [f64; 3], folder: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (mut made, mut failed) = (Vec::new(), Vec::new());
    if let Err(e) = std::fs::create_dir_all(folder) { failed.push(format!("{}: {e}", folder.display())); return (made, failed); }
    for preset in sizes {
        let path = folder.join(file_name(title, preset, format));
        let result = remake(document, preset, fit, background).and_then(|mut copy| match format { Format::Png => copy.export_png(&path), Format::Jpeg(q) => copy.export_jpeg(&path, q, background) });
        match result { Ok(()) => made.push(path), Err(e) => failed.push(format!("{}: {e:#}", preset.name)) }
    }
    (made, failed)
}

/// Every size added to `document` as an artboard in a row to the right, each holding a remade copy.
/// Returns the boards made and any size that failed.
pub fn add_as_artboards(document: &mut Document, sizes: &[SizePreset], fit: Fit, background: [f64; 3]) -> (Vec<uuid::Uuid>, Vec<String>) {
    let (mut made, mut failed) = (Vec::new(), Vec::new());
    let original = match document.duplicate() { Ok(d) => d, Err(e) => { failed.push(format!("{e:#}")); return (made, failed); } };
    for preset in sizes {
        let remade = match remake(&original, preset, fit, background) { Ok(d) => d, Err(e) => { failed.push(format!("{}: {e:#}", preset.name)); continue; } };
        let place = document.next_artboard_place(preset.width as f64, preset.height as f64);
        match document.import_as_artboard(&preset.name, &remade, place, Some(background)) { Ok(id) => made.push(id), Err(e) => failed.push(format!("{}: {e:#}", preset.name)) }
    }
    (made, failed)
}
