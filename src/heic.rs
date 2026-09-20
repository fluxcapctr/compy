//! HEIC and HEIF import through libheif, with the file's rotation and crop applied by the decoder.

use anyhow::{Context as _, Result, bail};
use cairo::ImageSurface;
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
use std::path::Path;

pub fn is_heic(path: &Path) -> bool { path.extension().is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif") || e.eq_ignore_ascii_case("hif")) }

/// The primary image as premultiplied ARGB with its size.
pub fn decode(path: &Path) -> Result<(ImageSurface, usize, usize)> {
    let name = path.to_str().context("the path is not valid text")?;
    let context = HeifContext::read_from_file(name).with_context(|| format!("reading {}", path.display()))?;
    let handle = context.primary_image_handle().context("the file has no primary image")?;
    let (w, h) = (handle.width() as usize, handle.height() as usize);
    if w == 0 || h == 0 || w > 30_000 || h > 30_000 || w * h > 100_000_000 { bail!("This image is {w} x {h}; sides run to 30,000 pixels and the whole to 100 megapixels."); }
    let image = LibHeif::new().decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None).context("decoding the HEIC image")?;
    let planes = image.planes();
    let Some(plane) = planes.interleaved else { bail!("the decoder gave no pixels") };
    let (pw, ph) = (plane.width as usize, plane.height as usize);
    if pw == 0 || ph == 0 { bail!("the decoder gave no pixels"); }
    if pw > 30_000 || ph > 30_000 || pw * ph > 100_000_000 || plane.data.len() < (ph - 1) * plane.stride + pw * 4 { bail!("This image is {pw} x {ph}; sides run to 30,000 pixels and the whole to 100 megapixels."); }
    let mut rgba = vec![0u8; pw * ph * 4];
    for y in 0..ph { rgba[y * pw * 4..(y + 1) * pw * 4].copy_from_slice(&plane.data[y * plane.stride..y * plane.stride + pw * 4]); }
    Ok((crate::png_io::from_straight_rgba(&rgba, pw, ph)?, pw, ph))
}
