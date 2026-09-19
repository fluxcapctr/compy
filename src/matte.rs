//! Remove Background: a segmentation model (ISNet, run through ONNX Runtime) says where the subject is, and the
//! refinements the Mac app applies to Vision's mask (`SubjectRemoval`, `GuidedMatte`) clean its edges: a guided
//! filter that pulls the mask onto the image's own edges, a contrast push, and an edge shift. The model is
//! downloaded once, on request, into the user's data directory.

use crate::raster::{new_argb, with_bytes, with_bytes_raw_mut};
use anyhow::{Context as _, Result, bail};
use cairo::ImageSurface;
use std::cell::RefCell;
use std::path::PathBuf;

pub const MODEL_URL: &str = "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx";
pub const MODEL_BYTES: u64 = 178_648_008;
const SIDE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatteSettings {
    /// Basic is the model's mask as it comes; Advanced applies the three refinements below.
    pub advanced: bool,
    /// Guided filter radius in layer pixels, 0 to 40.
    pub refine_edges: f64,
    /// 0 leaves the mask's grays; 100 is a hard cut at the middle.
    pub contrast: f64,
    /// Grows (positive) or shrinks the mask by this many pixels, -10 to 10.
    pub shift_edge: f64,
}

impl Default for MatteSettings { fn default() -> Self { MatteSettings { advanced: false, refine_edges: 12.0, contrast: 25.0, shift_edge: 0.0 } } }

impl MatteSettings {
    pub fn normalized(&self) -> MatteSettings {
        let c = |v: f64, lo: f64, hi: f64, d: f64| if v.is_finite() { v.clamp(lo, hi) } else { d };
        MatteSettings { advanced: self.advanced, refine_edges: c(self.refine_edges, 0.0, 40.0, 12.0), contrast: c(self.contrast, 0.0, 100.0, 25.0), shift_edge: c(self.shift_edge, -10.0, 10.0, 0.0) }
    }
}

pub fn model_path() -> PathBuf {
    if let Ok(p) = std::env::var("COMPOSITOR_MODEL") { return PathBuf::from(p); }
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
    base.join("compositor/models/isnet-general-use.onnx")
}

/// The model file is present and whole. A model named by `COMPOSITOR_MODEL` only has to be a real file.
pub fn model_ready() -> bool {
    let custom = std::env::var_os("COMPOSITOR_MODEL").is_some();
    std::fs::metadata(model_path()).is_ok_and(|m| if custom { m.len() > 1_000_000 } else { m.len() == MODEL_BYTES })
}

/// Drops a model that failed to load, so the dialog can fetch it again.
pub fn discard_model() { let _ = std::fs::remove_file(model_path()); }

/// Fetches the model to `model_path`, reporting (bytes so far, total) as it goes; the file appears whole or
/// not at all.
pub fn download_model(mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let target = model_path();
    if let Some(dir) = target.parent() { std::fs::create_dir_all(dir)?; }
    let staging = target.with_extension("onnx.part");
    let response = ureq::get(MODEL_URL).call().context("downloading the model")?;
    let total = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(MODEL_BYTES);
    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(&staging)?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = std::io::Read::read(&mut reader, &mut buffer)?;
        if n == 0 { break; }
        std::io::Write::write_all(&mut file, &buffer[..n])?;
        done += n as u64;
        progress(done, total);
    }
    drop(file);
    if done < 1_000_000 || (total > 0 && done != total) { let _ = std::fs::remove_file(&staging); bail!("the download ended early ({done} of {total} bytes)"); }
    std::fs::rename(&staging, &target)?;
    Ok(())
}

thread_local! { static SESSION: RefCell<Option<ort::session::Session>> = const { RefCell::new(None) }; }

/// The model's mask for `image`: 0 to 1 per pixel of the image's own grid, white over the subject.
pub fn subject_mask(image: &ImageSurface) -> Result<Vec<f32>> {
    if !model_ready() { bail!("The background removal model is not downloaded yet."); }
    let (w, h) = (image.width() as usize, image.height() as usize);
    // The image squashed to the model's square, over black where it is transparent.
    let square = new_argb(SIDE as i32, SIDE as i32)?;
    {
        let cr = cairo::Context::new(&square)?;
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.paint()?;
        cr.scale(SIDE as f64 / w as f64, SIDE as f64 / h as f64);
        cr.set_source_surface(image, 0.0, 0.0)?;
        cr.source().set_filter(cairo::Filter::Good);
        cr.paint()?;
    }
    let mut input = vec![0f32; 3 * SIDE * SIDE];
    with_bytes(&square, |d, stride| {
        for y in 0..SIDE { for x in 0..SIDE {
            let i = y * stride + x * 4;
            let (a, b, g, r) = (d[i + 3] as f32, d[i] as f32, d[i + 1] as f32, d[i + 2] as f32);
            let un = |c: f32| if a <= 0.0 { 0.0 } else { (c / a).min(1.0) };
            let o = y * SIDE + x;
            input[o] = un(r) - 0.5; input[SIDE * SIDE + o] = un(g) - 0.5; input[2 * SIDE * SIDE + o] = un(b) - 0.5;
        } }
    })?;
    let output: Vec<f32> = SESSION.with(|slot| -> Result<Vec<f32>> {
        let mut slot = slot.borrow_mut();
        // ort's errors carry non-Send builders, so they travel as text.
        if slot.is_none() {
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
            let builder = ort::session::Session::builder().map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut builder = builder.with_intra_threads(threads).map_err(|e| anyhow::anyhow!("{e}"))?;
            let session = builder.commit_from_file(model_path()).map_err(|e| anyhow::anyhow!("loading the background removal model: {e}"))?;
            *slot = Some(session);
        }
        let session = slot.as_mut().unwrap();
        let tensor = ort::value::Tensor::from_array(([1usize, 3, SIDE, SIDE], input)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let outputs = session.run(ort::inputs![tensor]).map_err(|e| anyhow::anyhow!("running the model: {e}"))?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(data[..SIDE * SIDE].to_vec())
    })?;
    // The model's range is not quite 0 to 1; stretch it (as rembg does), then back to the image's grid.
    let (lo, hi) = output.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    let span = (hi - lo).max(1e-6);
    let small = crate::raster::a8_filled(SIDE as i32, SIDE as i32, 0)?;
    with_bytes_raw_mut(&small, |d, stride| { for y in 0..SIDE { for x in 0..SIDE { d[y * stride + x] = (((output[y * SIDE + x] - lo) / span).clamp(0.0, 1.0) * 255.0).round() as u8; } } })?;
    let full = crate::raster::a8_filled(w as i32, h as i32, 0)?;
    {
        let cr = cairo::Context::new(&full)?;
        cr.scale(w as f64 / SIDE as f64, h as f64 / SIDE as f64);
        cr.set_source_surface(&small, 0.0, 0.0)?;
        cr.source().set_filter(cairo::Filter::Good);
        cr.set_operator(cairo::Operator::Source);
        cr.paint()?;
    }
    with_bytes(&full, |d, stride| (0..h).flat_map(|y| d[y * stride..y * stride + w].iter().map(|v| *v as f32 / 255.0)).collect())
}

/// Mean over a (2r+1) square, as two running-sum passes (`GuidedMatte.box`).
fn box_mean(source: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let span = (radius * 2 + 1) as f32;
    let r = radius as i64;
    let mut pass = vec![0f32; width * height];
    let cx = |x: i64| x.clamp(0, width as i64 - 1) as usize;
    let cy = |y: i64| y.clamp(0, height as i64 - 1) as usize;
    for y in 0..height {
        let row = y * width;
        let mut sum: f32 = (-r..=r).map(|x| source[row + cx(x)]).sum();
        for x in 0..width { pass[row + x] = sum / span; sum -= source[row + cx(x as i64 - r)]; sum += source[row + cx(x as i64 + r + 1)]; }
    }
    let mut out = vec![0f32; width * height];
    for x in 0..width {
        let mut sum: f32 = (-r..=r).map(|y| pass[cy(y) * width + x]).sum();
        for y in 0..height { out[y * width + x] = sum / span; sum -= pass[cy(y as i64 - r) * width + x]; sum += pass[cy(y as i64 + r + 1) * width + x]; }
    }
    out
}

/// `mask` pulled onto `guide`'s edges (He, Sun and Tang), both 0 to 1 (`GuidedMatte.filter`).
pub fn guided(mask: &[f32], guide: &[f32], width: usize, height: usize, radius: usize, epsilon: f32) -> Vec<f32> {
    let n = width * height;
    let mean_guide = box_mean(guide, width, height, radius);
    let mean_mask = box_mean(mask, width, height, radius);
    let squares: Vec<f32> = guide.iter().map(|g| g * g).collect();
    let products: Vec<f32> = guide.iter().zip(mask).map(|(g, m)| g * m).collect();
    let mean_squares = box_mean(&squares, width, height, radius);
    let mean_products = box_mean(&products, width, height, radius);
    let mut slope = vec![0f32; n];
    let mut offset = vec![0f32; n];
    for i in 0..n {
        let variance = mean_squares[i] - mean_guide[i] * mean_guide[i];
        let covariance = mean_products[i] - mean_guide[i] * mean_mask[i];
        slope[i] = covariance / (variance + epsilon);
        offset[i] = mean_mask[i] - slope[i] * mean_guide[i];
    }
    let mean_slope = box_mean(&slope, width, height, radius);
    let mean_offset = box_mean(&offset, width, height, radius);
    (0..n).map(|i| (mean_slope[i] * guide[i] + mean_offset[i]).clamp(0.0, 1.0)).collect()
}

/// The image's gray levels, 0 to 1, over `w` x `h` (the image's own grid).
fn gray_levels(image: &ImageSurface) -> Result<Vec<f32>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    with_bytes(image, |d, stride| (0..h).flat_map(|y| (0..w).map(move |x| { let i = y * stride + x * 4; (0.114 * d[i] as f32 + 0.587 * d[i + 1] as f32 + 0.299 * d[i + 2] as f32) / 255.0 })).collect())
}

/// The refinements over the raw mask (`SubjectRemoval.refined`), on the image's own grid. `limit` caps the
/// guided filter's working size for previews.
pub fn refine(mask: &[f32], image: &ImageSurface, settings: &MatteSettings, limit: usize) -> Result<Vec<f32>> {
    let s = settings.normalized();
    let (w, h) = (image.width() as usize, image.height() as usize);
    if !s.advanced { return Ok(mask.to_vec()); }
    let mut out = mask.to_vec();
    if s.refine_edges > 0.0 {
        // On a copy no larger than `limit`, the radius shrinking with it; fine detail comes from the guide either way.
        let factor = (limit as f64 / w.max(h) as f64).min(1.0);
        let (sw, sh) = (((w as f64 * factor).round() as usize).max(1), ((h as f64 * factor).round() as usize).max(1));
        let radius = ((s.refine_edges * factor).round() as usize).max(1);
        let guide = if (sw, sh) == (w, h) { gray_levels(image)? } else { resample_gray(&gray_levels(image)?, w, h, sw, sh) };
        let small_mask = if (sw, sh) == (w, h) { out.clone() } else { resample_gray(&out, w, h, sw, sh) };
        let refined = guided(&small_mask, &guide, sw, sh, radius, 1e-4);
        out = if (sw, sh) == (w, h) { refined } else { resample_gray(&refined, sw, sh, w, h) };
    }
    if s.shift_edge != 0.0 {
        // A blur then a hard threshold at the matching level moves the edge by the blur's reach.
        let reach = s.shift_edge.abs();
        let mut bytes: Vec<u8> = out.iter().map(|v| (v * 255.0).round() as u8).collect();
        crate::blur::gaussian(&mut bytes, w, h, 1, reach / 2.0);
        let level = if s.shift_edge < 0.0 { 0.75 } else { 0.25 };
        out = bytes.iter().map(|b| if *b as f32 / 255.0 >= level { 1.0 } else { 0.0 }).collect();
    }
    if s.contrast > 0.0 {
        let strength = (s.contrast / 100.0) as f32;
        let slope = 1.0 / (1.0 - strength * 0.98).max(0.02);
        for v in &mut out { *v = ((*v - 0.5) * slope + 0.5).clamp(0.0, 1.0); }
    }
    Ok(out)
}

/// Gray levels resampled between grids through cairo (bilinear).
fn resample_gray(levels: &[f32], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let from = crate::raster::a8_filled(w as i32, h as i32, 0).unwrap();
    let _ = with_bytes_raw_mut(&from, |d, stride| { for y in 0..h { for x in 0..w { d[y * stride + x] = (levels[y * w + x] * 255.0).round() as u8; } } });
    let to = crate::raster::a8_filled(tw as i32, th as i32, 0).unwrap();
    if let Ok(cr) = cairo::Context::new(&to) {
        cr.scale(tw as f64 / w as f64, th as f64 / h as f64);
        let _ = cr.set_source_surface(&from, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Good);
        cr.set_operator(cairo::Operator::Source);
        let _ = cr.paint();
    }
    with_bytes(&to, |d, stride| (0..th).flat_map(|y| d[y * stride..y * stride + tw].iter().map(|v| *v as f32 / 255.0)).collect()).unwrap_or_default()
}

/// 0 to 1 levels as an A8 mask.
pub fn a8_from_levels(levels: &[f32], w: usize, h: usize) -> Result<ImageSurface> {
    let out = crate::raster::a8_filled(w as i32, h as i32, 0)?;
    with_bytes_raw_mut(&out, |d, stride| { for y in 0..h { for x in 0..w { d[y * stride + x] = (levels[y * w + x].clamp(0.0, 1.0) * 255.0).round() as u8; } } })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guided_filter_follows_the_guide() {
        // A soft mask edge over a hard guide edge sharpens toward the guide.
        let (w, h) = (16, 4);
        let guide: Vec<f32> = (0..w * h).map(|i| if i % w < 8 { 0.0 } else { 1.0 }).collect();
        let mask: Vec<f32> = (0..w * h).map(|i| ((i % w) as f32 / 15.0)).collect();
        let out = guided(&mask, &guide, w, h, 2, 1e-4);
        assert!(out[8] - out[7] > 0.15 && out[7] < mask[7] && out[8] > mask[8], "sharper at the guide's edge: {:?}", &out[..16]);
        let s = MatteSettings { advanced: true, refine_edges: 0.0, contrast: 100.0, shift_edge: 0.0 };
        let image = new_argb(w as i32, h as i32).unwrap();
        let cut = refine(&mask, &image, &s, 2048).unwrap();
        assert_eq!((cut[0], cut[15]), (0.0, 1.0));
    }
}
