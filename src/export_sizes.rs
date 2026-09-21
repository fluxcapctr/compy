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
    format!("{}.{}", file_stem(title, preset), match format { Format::Png => "png", Format::Jpeg(_) => "jpg" })
}

fn file_stem(title: &str, preset: &SizePreset) -> String {
    format!("{}-{}-{}x{}", crate::document::clean_file_name(title), crate::document::clean_file_name(&preset.name), preset.width, preset.height)
}

/// A copy of `document` remade for `preset` in `fit`, ready to render.
pub fn remake(document: &Document, preset: &SizePreset, fit: Fit, background: [f64; 3]) -> Result<Document> {
    let (w, h) = (preset.width, preset.height);
    if w < 1 || h < 1 || w > 30_000 || h > 30_000 || w as i64 * h as i64 > 100_000_000 { bail!("{} x {} is not a size that can be made", w, h); }
    let (dw, dh) = (document.width() as f64, document.height() as f64);
    let (rw, rh) = (w as f64 / dw, h as f64 / dh);
    let scale = match fit { Fit::Pad => rw.min(rh), Fit::Fill | Fit::Reframe => rw.max(rh) };
    let mut copy = document.duplicate()?;
    let elements: Vec<(uuid::Uuid, (f64, f64, f64, f64))> = if fit == Fit::Reframe { element_layers(&mut copy)? } else { Vec::new() };
    let (sw, sh) = (((dw * scale).round() as i32).max(1), ((dh * scale).round() as i32).max(1));
    if (sw, sh) != (dw as i32, dh as i32) { copy.image_size(sw, sh, copy.renderer.resolution(), crate::format::Sampling::High)?; }
    let fill = if fit == Fit::Pad { Some(background) } else { None };
    // The scaled copy centered in the frame: cropped (fill) or padded (pad).
    let offset = (((w - sw) as f64 / 2.0).floor(), ((h - sh) as f64 / 2.0).floor());
    copy.canvas_size(w, h, 4, fill, Some(offset), "Export Size")?;
    if fit == Fit::Reframe {
        // The elements go back where they sat, in the frame's proportions, at the short side's scale, and
        // inside a five percent margin.
        let short = rw.min(rh);
        let margin = (w.min(h) as f64 * 0.05).round();
        let inside = (w as f64 - 2.0 * margin, h as f64 - 2.0 * margin);
        for (id, (vx0, vy0, vx1, vy1)) in elements {
            if !copy.has_layer(id) { continue; }
            // The visible box (a logo in a big clear layer, or what a mask leaves) is what gets placed:
            // where its center sat, as a fraction of the original canvas, at the short side's scale,
            // shrunk to the frame's inside if it is bigger.
            let original = document.renderer.layer(id).transform;
            let (vw, vh) = ((vx1 - vx0).max(1.0), (vy1 - vy0).max(1.0));
            let k = short * (inside.0 / (vw * short)).min(inside.1 / (vh * short)).min(1.0);
            let (tw, th) = (vw * k, vh * k);
            let (cx, cy) = ((vx0 + vx1) / 2.0 / dw, (vy0 + vy1) / 2.0 / dh);
            let vx = (cx * w as f64 - tw / 2.0).clamp(margin, (w as f64 - margin - tw).max(margin));
            let vy = (cy * h as f64 - th / 2.0).clamp(margin, (h as f64 - margin - th).max(margin));
            // The layer's transform follows its visible box: the offset between the two scales with it.
            let oc = original.center();
            let off = ((vx0 + vx1) / 2.0 - oc.0, (vy0 + vy1) / 2.0 - oc.1);
            let size = crate::format::Size((original.size.0 * k).max(1.0), (original.size.1 * k).max(1.0));
            let center = (vx + tw / 2.0 - off.0 * k, vy + th / 2.0 - off.1 * k);
            let mut moved = copy.renderer.layer(id).transform;
            moved.size = size;
            moved.origin = crate::format::Point((center.0 - size.0 / 2.0).round(), (center.1 - size.1 / 2.0).round());
            copy.set_transform(id, moved, "Reframe");
            // The element's style follows it: the copy was scaled by `scale`, the element ends up at `k`.
            if let Some(e) = copy.renderer.layer(id).effects.as_ref().and_then(crate::effects::Effects::from_record) { copy.renderer.set_effects(id, Some(e.scaled(k / scale).to_record())); }
        }
    }
    Ok(copy)
}

/// The layers that are elements rather than the picture: type, and anything whose visible pixels
/// (through its mask) cover less than half the canvas; with the box around those pixels.
fn element_layers(document: &mut Document) -> Result<Vec<(uuid::Uuid, (f64, f64, f64, f64))>> {
    let candidates: Vec<(uuid::Uuid, bool)> = crate::format::visible_layers(document.renderer.layers()).into_iter().map(|id| (id, document.renderer.layer(id).text.is_some())).filter(|(id, _)| document.renderer.layer(*id).adjustment.is_none()).collect();
    let area = document.width() as f64 * document.height() as f64;
    let mut out = Vec::new();
    for (id, is_text) in candidates {
        let Some((fraction, bounds)) = document.layer_coverage(id)? else { continue };
        // Type is always an element; anything else is the picture once it covers half the canvas (a
        // background rectangle counts as the picture, a small shape as an element). So is anything whose
        // visible pixels reach across most of the canvas however sparse they are: a vignette, a frame, a
        // border, a scatter of dust.
        let span = ((bounds.2 - bounds.0).min(document.width() as f64) * (bounds.3 - bounds.1).min(document.height() as f64)) / area;
        if is_text || (fraction < 0.5 && span < 0.6) { out.push((id, bounds)); }
    }
    Ok(out)
}

/// Writes every size into `folder`, returning the files made and any size that failed.
pub fn export_all(document: &Document, title: &str, sizes: &[SizePreset], fit: Fit, format: Format, background: [f64; 3], folder: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (mut made, mut failed) = (Vec::new(), Vec::new());
    if let Err(e) = std::fs::create_dir_all(folder) { failed.push(format!("{}: {e}", folder.display())); return (made, failed); }
    let mut used = std::collections::HashSet::new();
    for preset in sizes {
        let path = crate::document::unique_file(folder, &file_stem(title, preset), match format { Format::Png => "png", Format::Jpeg(_) => "jpg" }, &mut used);
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
    // One step in the history for the whole batch.
    document.begin_edit("Export Sizes as Artboards");
    for preset in sizes {
        let remade = match remake(&original, preset, fit, background) { Ok(d) => d, Err(e) => { failed.push(format!("{}: {e:#}", preset.name)); continue; } };
        let place = document.next_artboard_place(preset.width as f64, preset.height as f64);
        match document.import_as_artboard(&preset.name, &remade, place, Some(background)) { Ok(id) => made.push(id), Err(e) => failed.push(format!("{}: {e:#}", preset.name)) }
    }
    if made.is_empty() { document.abort_edit(); } else { document.end_edit(); }
    (made, failed)
}
