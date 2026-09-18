//! Bindings to the C pixel core in `csrc/`. Every function there takes premultiplied 4-byte pixels with an
//! explicit stride, alpha in the fourth byte. Cairo's ARGB32 is B, G, R, A in memory on little-endian machines:
//! routines that treat the three color channels alike work on Cairo buffers as they are; the few that read
//! luminance get their red and blue swapped around the call (`swap_red_blue`).

use std::ffi::c_int;

unsafe extern "C" {
    fn layer_extract_alpha(rgba: *const u8, rgba_stride: usize, gray: *mut u8, gray_stride: usize, width: usize, height: usize);
    fn layer_unpremultiply_opaque(rgba: *mut u8, stride: usize, width: usize, height: usize);
    fn layer_restore_alpha(rgba: *mut u8, stride: usize, alpha: *const u8, alpha_stride: usize, width: usize, height: usize);
    fn rgba_clamp_premultiplied(rgba: *mut u8, count: usize);
    fn adjust_gradient_map(rgba: *mut u8, width: usize, height: usize, stride: usize, table: *const u8);
    fn adjust_grain(rgba: *mut u8, width: usize, height: usize, stride: usize, amount: f64, size: f64, roughness: f64, seed: u32, origin_x: f64, origin_y: f64, units_per_pixel: f64);
    fn noise_add(rgba: *mut u8, width: usize, height: usize, stride: usize, amount: f32, gaussian: c_int, monochromatic: c_int, seed: u32);
    fn lens_distort(source: *const u8, destination: *mut u8, width: usize, height: usize, stride: usize, k: f64);
    fn levels_apply(pixels: *mut u8, count: usize, tables: *const f32);
    fn levels_histogram(pixels: *const u8, coverage: *const u8, count: usize, bins: *mut f64);
    fn content_fill(rgba: *mut u8, stride: usize, mask: *const u8, mask_stride: usize, width: c_int, height: c_int) -> c_int;
    fn spot_heal(rgba: *mut u8, coverage: *const u8, width: usize, height: usize, stride: usize, opacity: f32, mode: c_int, seed: u32) -> c_int;
    fn heal_coverage_bounds(gray: *const u8, width: usize, height: usize, stride: usize, bounds: *mut std::ffi::c_long);
    fn brush_alpha_bounds(bytes: *const u8, width: usize, height: usize, stride: usize, bounds: *mut usize);
    fn wand_mask(rgba: *const u8, width: usize, height: usize, stride: usize, seed_x: usize, seed_y: usize, radius: usize, tolerance: c_int, contiguous: c_int, mask: *mut u8) -> std::ffi::c_long;
    fn wand_trace(mask: *const u8, width: usize, height: usize, points: *mut *mut i32, point_count: *mut usize, loops: *mut *mut i32, loop_count: *mut usize) -> c_int;
    fn free(ptr: *mut std::ffi::c_void);
}

fn check(len: usize, stride: usize, width: usize, height: usize, bytes_per_pixel: usize) {
    assert!(stride >= width * bytes_per_pixel, "stride {stride} shorter than a row of {width} pixels");
    assert!(height == 0 || len >= stride * (height - 1) + width * bytes_per_pixel, "buffer too small");
}

/// Copies each pixel's alpha into a gray buffer.
pub fn extract_alpha(rgba: &[u8], rgba_stride: usize, gray: &mut [u8], gray_stride: usize, width: usize, height: usize) {
    check(rgba.len(), rgba_stride, width, height, 4);
    check(gray.len(), gray_stride, width, height, 1);
    unsafe { layer_extract_alpha(rgba.as_ptr(), rgba_stride, gray.as_mut_ptr(), gray_stride, width, height) }
}

/// Divides colors by alpha and sets alpha to 255: the layer's colors at full coverage.
pub fn unpremultiply_opaque(rgba: &mut [u8], stride: usize, width: usize, height: usize) {
    check(rgba.len(), stride, width, height, 4);
    unsafe { layer_unpremultiply_opaque(rgba.as_mut_ptr(), stride, width, height) }
}

/// Multiplies colors by `alpha` and stores it as the pixels' alpha.
pub fn restore_alpha(rgba: &mut [u8], stride: usize, alpha: &[u8], alpha_stride: usize, width: usize, height: usize) {
    check(rgba.len(), stride, width, height, 4);
    check(alpha.len(), alpha_stride, width, height, 1);
    unsafe { layer_restore_alpha(rgba.as_mut_ptr(), stride, alpha.as_ptr(), alpha_stride, width, height) }
}

/// Clamps each color channel to its pixel's alpha, for `count` contiguous pixels.
pub fn clamp_premultiplied(rgba: &mut [u8], count: usize) {
    assert!(rgba.len() >= count * 4);
    unsafe { rgba_clamp_premultiplied(rgba.as_mut_ptr(), count) }
}

/// Swaps the first and third byte of every pixel: Cairo's BGRA to the RGBA the luminance routines read.
pub fn swap_red_blue(rgba: &mut [u8], stride: usize, width: usize, height: usize) {
    for y in 0..height {
        let row = &mut rgba[y * stride..y * stride + width * 4];
        for p in row.chunks_exact_mut(4) { p.swap(0, 2); }
    }
}

/// Gradient Map: each pixel's luminance picks a color from `table` (256 x 3 straight sRGB bytes, darkest first).
/// Expects RGBA byte order.
pub fn gradient_map(rgba: &mut [u8], width: usize, height: usize, stride: usize, table: &[u8; 768]) {
    check(rgba.len(), stride, width, height, 4);
    unsafe { adjust_gradient_map(rgba.as_mut_ptr(), width, height, stride, table.as_ptr()) }
}

/// Film grain, position-seeded in document space. Expects RGBA byte order.
#[allow(clippy::too_many_arguments)]
pub fn grain(rgba: &mut [u8], width: usize, height: usize, stride: usize, amount: f64, size: f64, roughness: f64, seed: u32, origin: (f64, f64), units_per_pixel: f64) {
    check(rgba.len(), stride, width, height, 4);
    unsafe { adjust_grain(rgba.as_mut_ptr(), width, height, stride, amount, size, roughness, seed, origin.0, origin.1, units_per_pixel) }
}

/// Add Noise: `amount` is Photoshop's percentage.
pub fn noise(rgba: &mut [u8], width: usize, height: usize, stride: usize, amount: f32, gaussian: bool, monochromatic: bool, seed: u32) {
    check(rgba.len(), stride, width, height, 4);
    unsafe { noise_add(rgba.as_mut_ptr(), width, height, stride, amount, gaussian as c_int, monochromatic as c_int, seed) }
}

/// Radial lens distortion from `source` into `destination` (same layout), `k` > 0 straightening barrel distortion.
pub fn lens(source: &[u8], destination: &mut [u8], width: usize, height: usize, stride: usize, k: f64) {
    check(source.len(), stride, width, height, 4);
    check(destination.len(), stride, width, height, 4);
    unsafe { lens_distort(source.as_ptr(), destination.as_mut_ptr(), width, height, stride, k) }
}

/// Levels: `tables` holds 3 x 256 floats, one lookup per channel in the buffer's own byte order, applied to
/// `count` contiguous pixels.
pub fn levels(pixels: &mut [u8], count: usize, tables: &[f32; 768]) {
    assert!(pixels.len() >= count * 4);
    unsafe { levels_apply(pixels.as_mut_ptr(), count, tables.as_ptr()) }
}

/// Histograms of `count` contiguous pixels: 4 x 256 bins (mean of the channels, then each channel in the
/// buffer's byte order), counting only pixels with nonzero `coverage` when given.
pub fn histogram(pixels: &[u8], coverage: Option<&[u8]>, count: usize) -> [f64; 1024] {
    assert!(pixels.len() >= count * 4);
    if let Some(c) = coverage { assert!(c.len() >= count); }
    let mut bins = [0f64; 1024];
    unsafe { levels_histogram(pixels.as_ptr(), coverage.map_or(std::ptr::null(), |c| c.as_ptr()), count, bins.as_mut_ptr()) }
    bins
}

/// Content-Aware Fill of the pixels `mask` marks, in place. Ok(false) when no source patch exists.
pub fn fill(rgba: &mut [u8], stride: usize, mask: &[u8], mask_stride: usize, width: usize, height: usize) -> anyhow::Result<bool> {
    check(rgba.len(), stride, width, height, 4);
    check(mask.len(), mask_stride, width, height, 1);
    match unsafe { content_fill(rgba.as_mut_ptr(), stride, mask.as_ptr(), mask_stride, width as c_int, height as c_int) } {
        1 => Ok(true),
        0 => Ok(false),
        _ => anyhow::bail!("not enough memory for Content-Aware Fill"),
    }
}

/// Spot healing of the pixels `coverage` (width x height bytes, packed) marks, in place. Mode 0 is
/// Content-Aware, 1 Create Texture, 2 Proximity Match.
pub fn heal(rgba: &mut [u8], coverage: &[u8], width: usize, height: usize, stride: usize, opacity: f32, mode: i32, seed: u32) -> anyhow::Result<()> {
    check(rgba.len(), stride, width, height, 4);
    assert!(coverage.len() >= width * height);
    if unsafe { spot_heal(rgba.as_mut_ptr(), coverage.as_ptr(), width, height, stride, opacity, mode, seed) } != 0 {
        anyhow::bail!("not enough memory for Spot Healing");
    }
    Ok(())
}

/// Half-open bounds (left, top, right, bottom) of the nonzero bytes of a gray buffer; None when empty.
pub fn gray_bounds(gray: &[u8], width: usize, height: usize, stride: usize) -> Option<(usize, usize, usize, usize)> {
    check(gray.len(), stride, width, height, 1);
    let mut bounds = [0 as std::ffi::c_long; 4];
    unsafe { heal_coverage_bounds(gray.as_ptr(), width, height, stride, bounds.as_mut_ptr()) }
    (bounds[2] > bounds[0] && bounds[3] > bounds[1]).then(|| (bounds[0] as usize, bounds[1] as usize, bounds[2] as usize, bounds[3] as usize))
}

/// Half-open bounds of the pixels with nonzero alpha; None when fully transparent.
pub fn alpha_bounds(rgba: &[u8], width: usize, height: usize, stride: usize) -> Option<(usize, usize, usize, usize)> {
    check(rgba.len(), stride, width, height, 4);
    let mut bounds = [0usize; 4];
    unsafe { brush_alpha_bounds(rgba.as_ptr(), width, height, stride, bounds.as_mut_ptr()) }
    (bounds[2] > bounds[0] && bounds[3] > bounds[1]).then_some((bounds[0], bounds[1], bounds[2], bounds[3]))
}

/// Magic Wand match around the seed pixel, written as 255/0 into `mask` (width x height, packed). Returns
/// how many pixels matched.
#[allow(clippy::too_many_arguments)]
pub fn wand(rgba: &[u8], width: usize, height: usize, stride: usize, seed: (usize, usize), radius: usize, tolerance: i32, contiguous: bool, mask: &mut [u8]) -> anyhow::Result<usize> {
    check(rgba.len(), stride, width, height, 4);
    assert!(mask.len() >= width * height);
    let count = unsafe { wand_mask(rgba.as_ptr(), width, height, stride, seed.0, seed.1, radius, tolerance, contiguous as c_int, mask.as_mut_ptr()) };
    if count < 0 { anyhow::bail!("not enough memory for the Magic Wand"); }
    Ok(count as usize)
}

/// The outline of a packed mask's nonzero pixels as closed loops of corner points along pixel edges (outer
/// boundaries clockwise, holes counterclockwise). Ok(None) when the outline is too detailed to draw.
pub fn trace(mask: &[u8], width: usize, height: usize) -> anyhow::Result<Option<Vec<Vec<(i32, i32)>>>> {
    assert!(mask.len() >= width * height);
    let mut points: *mut i32 = std::ptr::null_mut();
    let mut loops: *mut i32 = std::ptr::null_mut();
    let (mut point_count, mut loop_count) = (0usize, 0usize);
    let status = unsafe { wand_trace(mask.as_ptr(), width, height, &mut points, &mut point_count, &mut loops, &mut loop_count) };
    let result = match status {
        0 => {
            let mut out = Vec::with_capacity(loop_count);
            let mut index = 0usize;
            for l in 0..loop_count {
                let length = unsafe { *loops.add(l) } as usize;
                let mut corners = Vec::with_capacity(length);
                for c in index..index + length {
                    corners.push(unsafe { (*points.add(c * 2), *points.add(c * 2 + 1)) });
                }
                out.push(corners);
                index += length;
            }
            Ok(Some(out))
        }
        -2 => Ok(None),
        _ => Err(anyhow::anyhow!("not enough memory to outline the selection")),
    };
    unsafe { free(points.cast()); free(loops.cast()); }
    result
}
