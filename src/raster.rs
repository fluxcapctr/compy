//! Pixel buffers as Cairo image surfaces: premultiplied ARGB32 for layer images and the canvas, A8 for masks
//! (255 reveals, 0 hides), plus the sharp 2x reduction the renderer draws large shrinks from.

use anyhow::{Context as _, Result, bail};
use cairo::{Format, ImageSurface};
use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::LazyLock;

// Cairo hands out one pixel pointer per surface however many handles share it, so the raw accessors below
// keep their own ledger: any number of readers, or one writer, per surface on this thread. A nested access
// that would alias is refused instead of handing out overlapping slices.
thread_local! { static BORROWS: RefCell<HashMap<usize, i32>> = RefCell::new(HashMap::new()); }

struct Borrow(usize, i32);
impl Borrow {
    /// `delta` is +1 for a reader, -1 for the single writer.
    fn take(ptr: usize, delta: i32) -> Result<Borrow> {
        BORROWS.with(|b| {
            let mut b = b.borrow_mut();
            let count = b.entry(ptr).or_insert(0);
            let allowed = if delta > 0 { *count >= 0 } else { *count == 0 };
            if !allowed { bail!("the surface's pixels are already borrowed {}", if *count < 0 { "for writing" } else { "for reading" }); }
            *count += delta;
            Ok(Borrow(ptr, delta))
        })
    }
}
impl Drop for Borrow {
    fn drop(&mut self) { BORROWS.with(|b| { let mut b = b.borrow_mut(); if let Some(c) = b.get_mut(&self.0) { *c -= self.1; if *c == 0 { b.remove(&self.0); } } }); }
}

/// A transparent premultiplied ARGB32 surface.
pub fn new_argb(width: i32, height: i32) -> Result<ImageSurface> {
    Ok(ImageSurface::create(Format::ARgb32, width, height)?)
}

/// An A8 surface holding `data` rows of `stride` bytes.
pub fn a8_from_data(width: i32, height: i32, data: Vec<u8>, stride: i32) -> Result<ImageSurface> {
    Ok(ImageSurface::create_for_data(data, Format::A8, width, height, stride)?)
}

/// An A8 surface filled with one value.
pub fn a8_filled(width: i32, height: i32, value: u8) -> Result<ImageSurface> {
    let stride = Format::A8.stride_for_width(width as u32)?;
    a8_from_data(width, height, vec![value; stride as usize * height as usize], stride)
}

/// An ARGB32 surface from premultiplied B, G, R, A bytes packed `width * 4` per row.
pub fn argb_from_packed(width: i32, height: i32, packed: Vec<u8>) -> Result<ImageSurface> {
    let stride = Format::ARgb32.stride_for_width(width as u32)?;
    assert_eq!(stride, width * 4, "Cairo pads ARGB32 rows to 4 bytes, which width * 4 already is");
    Ok(ImageSurface::create_for_data(packed, Format::ARgb32, width, height, stride)?)
}

/// Reads a surface's bytes. Goes through Cairo's raw accessor rather than `data()`, so a surface that undo
/// history or a pattern also references can still be read; surfaces are never written after they are built.
pub fn with_bytes<R>(surface: &ImageSurface, f: impl FnOnce(&[u8], usize) -> R) -> Result<R> {
    surface.flush();
    let stride = surface.stride() as usize;
    let len = stride * surface.height() as usize;
    let ptr = unsafe { cairo::ffi::cairo_image_surface_get_data(surface.to_raw_none()) };
    if ptr.is_null() { bail!("surface has no pixel data"); }
    let _borrow = Borrow::take(ptr as usize, 1)?;
    let data = unsafe { std::slice::from_raw_parts(ptr, len) };
    Ok(f(data, stride))
}

/// Writes a surface's bytes in place; Cairo is told the pixels changed when the borrow ends.
pub fn with_bytes_mut<R>(surface: &mut ImageSurface, f: impl FnOnce(&mut [u8], usize) -> R) -> Result<R> {
    surface.flush();
    let stride = surface.stride() as usize;
    let mut data = surface.data().context("surface is still referenced by a pattern or context")?;
    Ok(f(&mut data, stride))
}

/// Halvings to draw from when an image lands `factor` output pixels per image pixel: the most that still
/// leave the copy at least that large (0 from half size up). Mirrors `DownsampleCache.level(for:)`.
pub fn level_for(factor: f64) -> usize {
    if !factor.is_finite() || factor <= 0.0 || factor >= 0.5 { return 0; }
    ((1.0 / factor).log2().floor() as usize).min(MAX_LEVEL)
}
pub const MAX_LEVEL: usize = 6;

fn lanczos3(x: f64) -> f64 {
    if x == 0.0 { return 1.0; }
    if x.abs() >= 3.0 { return 0.0; }
    let px = PI * x;
    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
}

/// Twelve taps reducing a row by exactly two: output pixel `i` is centered between input pixels `2i` and
/// `2i + 1`, and tap `t` reads input pixel `2i + t - 5`.
static TAPS: LazyLock<[f32; 12]> = LazyLock::new(|| {
    let mut weights = [0f64; 12];
    for (t, w) in weights.iter_mut().enumerate() {
        let offset = t as f64 - 5.0;
        *w = lanczos3((offset - 0.5) / 2.0);
    }
    let sum: f64 = weights.iter().sum();
    let mut out = [0f32; 12];
    for (o, w) in out.iter_mut().zip(weights) { *o = (w / sum) as f32; }
    out
});

/// Exactly half the size, rounded up, with Lanczos resampling, as `DownsampleCache.halve` does. Color
/// images read transparent beyond their edges so cut edges fade the same wherever they are cut, and
/// ringing past a pixel's alpha is clamped so edges don't glow. Masks repeat their edge pixels instead.
pub fn halve(source: &ImageSurface) -> Result<ImageSurface> {
    let format = source.format();
    let (w, h) = (source.width() as usize, source.height() as usize);
    let (ow, oh) = (w.div_ceil(2), h.div_ceil(2));
    let destination = ImageSurface::create(format, ow as i32, oh as i32)?;
    halve_into(source, &destination, (0, 0, ow as i32, oh as i32))?;
    Ok(destination)
}

/// The destination pixels a change to source pixels [a, b) touches, through the halving's 12 taps.
pub fn halved_span(a: i32, b: i32) -> (i32, i32) { ((a - 6).div_euclid(2), (b + 6).div_euclid(2) + 1) }

/// Recomputes `destination`'s pixels inside `region` (x0, y0, x1, y1, half open) from `source`, which is
/// exactly twice its size (rounded up). The same arithmetic as a whole halving, so a region redone after its
/// source changed matches what a fresh halving would give: this is what keeps a live stroke's reduced
/// copies in step with the pixels underneath (`TiledLayerRenderer`'s pieces, done as arithmetic).
pub fn halve_into(source: &ImageSurface, destination: &ImageSurface, region: (i32, i32, i32, i32)) -> Result<()> {
    let format = source.format();
    let channels = match format {
        Format::ARgb32 => 4usize,
        Format::A8 => 1usize,
        _ => bail!("halve: unsupported surface format {format:?}"),
    };
    if destination.format() != format { bail!("halve: destination format differs"); }
    let (w, h) = (source.width() as usize, source.height() as usize);
    let (ow, oh) = (destination.width() as usize, destination.height() as usize);
    if ow != w.div_ceil(2) || oh != h.div_ceil(2) { bail!("halve: destination is not half the source"); }
    let (x0, y0) = (region.0.max(0) as usize, region.1.max(0) as usize);
    let (x1, y1) = ((region.2.max(0) as usize).min(ow), (region.3.max(0) as usize).min(oh));
    if x1 <= x0 || y1 <= y0 { return Ok(()); }
    let pad_transparent = channels == 4;
    let taps = *TAPS;
    // Source rows the region's vertical taps reach.
    let (sy0, sy1) = ((2 * y0 as isize - 5).max(0) as usize, (2 * (y1 - 1) + 7).min(h));
    let rows = sy1 - sy0;
    let cols = x1 - x0;
    let out_stride = destination.stride() as usize;
    let mut tmp = vec![0f32; rows * cols * channels];
    with_bytes(source, |data, stride| {
        // Horizontal pass over just the rows and columns the region needs.
        for (r, y) in (sy0..sy1).enumerate() {
            let row = &data[y * stride..y * stride + w * channels];
            let trow = &mut tmp[r * cols * channels..(r + 1) * cols * channels];
            for (c, ox) in (x0..x1).enumerate() {
                let acc = &mut trow[c * channels..(c + 1) * channels];
                for (t, &weight) in taps.iter().enumerate() {
                    let k = 2 * ox as isize + t as isize - 5;
                    let k = if k < 0 || k >= w as isize {
                        if pad_transparent { continue; }
                        k.clamp(0, w as isize - 1) as usize
                    } else { k as usize };
                    for ch in 0..channels { acc[ch] += weight * row[k * channels + ch] as f32; }
                }
            }
        }
    })?;
    let mut out = vec![0u8; (y1 - y0) * cols * channels];
    for (r, oy) in (y0..y1).enumerate() {
        let orow = &mut out[r * cols * channels..(r + 1) * cols * channels];
        for c in 0..cols {
            for ch in 0..channels {
                let mut acc = 0f32;
                for (t, &weight) in taps.iter().enumerate() {
                    let k = 2 * oy as isize + t as isize - 5;
                    let k = if k < 0 || k >= h as isize {
                        if pad_transparent { continue; }
                        k.clamp(0, h as isize - 1) as usize
                    } else { k as usize };
                    acc += weight * tmp[((k - sy0) * cols + c) * channels + ch];
                }
                orow[c * channels + ch] = (acc + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
    if channels == 4 { crate::ffi::clamp_premultiplied(&mut out, (y1 - y0) * cols); }
    with_bytes_raw_mut(destination, |dst, _| {
        for (r, oy) in (y0..y1).enumerate() {
            dst[oy * out_stride + x0 * channels..oy * out_stride + x1 * channels].copy_from_slice(&out[r * cols * channels..(r + 1) * cols * channels]);
        }
    })?;
    Ok(())
}

/// Writes a surface's bytes through Cairo's raw accessor, then marks it changed. For surfaces this crate
/// owns and updates in place (a stroke's preview and its reduced copies) while patterns still reference them;
/// nothing else may be drawing with the surface at the time.
pub fn with_bytes_raw_mut<R>(surface: &ImageSurface, f: impl FnOnce(&mut [u8], usize) -> R) -> Result<R> {
    surface.flush();
    let stride = surface.stride() as usize;
    let len = stride * surface.height() as usize;
    let ptr = unsafe { cairo::ffi::cairo_image_surface_get_data(surface.to_raw_none()) };
    if ptr.is_null() { bail!("surface has no pixel data"); }
    let _borrow = Borrow::take(ptr as usize, -1)?;
    let data = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
    let result = f(data, stride);
    surface.mark_dirty();
    Ok(result)
}
