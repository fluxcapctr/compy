//! Free distortion (Ctrl-drag a transform handle): a layer's four corners move independently. Layer transforms
//! are affine, so the picture is resampled into the new shape through a perspective mapping (`DistortWarp`),
//! leaving an ordinary axis-aligned layer over the shape's bounds.

use crate::format::{Point, Size, Transform};
use crate::raster::{new_argb, with_bytes, with_bytes_raw_mut};
use anyhow::{Result, bail};
use cairo::ImageSurface;

pub type Corners = [(f64, f64); 4];

/// The transform's corners in handle order: top left, top right, bottom right, bottom left.
pub fn corners(transform: &Transform) -> Corners {
    [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|u| transform.point(u))
}

/// Four finite corners making a convex, non-degenerate shape; a twisted (bow-tie) or collapsed shape has no
/// sensible warp and is refused.
pub fn is_usable(c: &Corners) -> bool {
    if !c.iter().all(|p| p.0.is_finite() && p.1.is_finite() && p.0.abs() <= 1_000_000.0 && p.1.abs() <= 1_000_000.0) { return false; }
    let mut sign = 0.0;
    for i in 0..4 {
        let (a, b, d) = (c[i], c[(i + 1) % 4], c[(i + 2) % 4]);
        let cross = (b.0 - a.0) * (d.1 - b.1) - (b.1 - a.1) * (d.0 - b.0);
        if cross.abs() <= 0.01 { return false; }
        if sign == 0.0 { sign = cross.signum(); } else if cross.signum() != sign { return false; }
    }
    true
}

/// The perspective mapping of the unit square onto `c`, as a 3 x 3 matrix (row major) taking (u, v, 1).
pub fn homography(c: &Corners) -> [f64; 9] {
    let (sx, sy) = (c[0].0 - c[1].0 + c[2].0 - c[3].0, c[0].1 - c[1].1 + c[2].1 - c[3].1);
    let (mut g, mut h) = (0.0, 0.0);
    if sx.abs() > 1e-9 || sy.abs() > 1e-9 {
        let (dx1, dx2, dy1, dy2) = (c[1].0 - c[2].0, c[3].0 - c[2].0, c[1].1 - c[2].1, c[3].1 - c[2].1);
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() > 1e-12 { g = (sx * dy2 - dx2 * sy) / den; h = (dx1 * sy - sx * dy1) / den; }
    }
    let (a, b, x0) = (c[1].0 - c[0].0 + g * c[1].0, c[3].0 - c[0].0 + h * c[3].0, c[0].0);
    let (d, e, y0) = (c[1].1 - c[0].1 + g * c[1].1, c[3].1 - c[0].1 + h * c[3].1, c[0].1);
    [a, b, x0, d, e, y0, g, h, 1.0]
}

pub fn apply(m: &[f64; 9], p: (f64, f64)) -> (f64, f64) {
    let w = m[6] * p.0 + m[7] * p.1 + m[8];
    ((m[0] * p.0 + m[1] * p.1 + m[2]) / w, (m[3] * p.0 + m[4] * p.1 + m[5]) / w)
}

pub fn invert(m: &[f64; 9]) -> Option<[f64; 9]> {
    let (a, b, c, d, e, f, g, h, i) = (m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 { return None; }
    Some([(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det,
          (f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det,
          (d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det])
}

/// Where `placement`'s corners land when the perspective taking `transform`'s corners to `corners` is applied
/// around it too: how a linked mask placed apart from its layer distorts with the layer (`carried`).
pub fn carried(placement: &Transform, transform: &Transform, to: &Corners) -> Option<Corners> {
    let to_unit = transform.unit_to_document().try_invert().ok()?;
    let map = homography(to);
    let mut out = [(0.0, 0.0); 4];
    for (i, p) in corners(placement).iter().enumerate() { out[i] = apply(&map, to_unit.transform_point(p.0, p.1)); }
    Some(out)
}

/// The whole-pixel bounds of the shape and the axis-aligned transform over them.
fn placed(c: &Corners, sampling: crate::format::Sampling) -> Result<(i32, i32, i32, i32, Transform)> {
    let (min_x, min_y) = (c.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor(), c.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor());
    let (max_x, max_y) = (c.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil(), c.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil());
    let (w, h) = (max_x - min_x, max_y - min_y);
    if w < 1.0 || h < 1.0 || w > 30_000.0 || h > 30_000.0 || w * h > 100_000_000.0 { bail!("The distorted shape is larger than the 30,000-pixel side or 100-megapixel limit."); }
    Ok((min_x as i32, min_y as i32, w as i32, h as i32, Transform { origin: Point(min_x, min_y), size: Size(w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling }))
}

/// `image` (premultiplied ARGB, or A8 with `is_mask`), shown through `transform`, resampled so its corners land
/// on `corners`. Returns the warped pixels over the shape's whole-pixel bounds and the transform placing them.
/// `limit` caps the longest side, for previews.
pub fn warp(image: &ImageSurface, transform: &Transform, c: &Corners, is_mask: bool, limit: Option<f64>) -> Result<(ImageSurface, Transform)> {
    if !is_usable(c) { bail!("The shape is twisted or collapsed."); }
    let (bx, by, bw, bh, placed) = placed(c, transform.sampling)?;
    if is_mask && image.width() == 1 && image.height() == 1 { return Ok((image.clone(), placed)); }
    let factor = limit.map_or(1.0, |l| (l / bw.max(bh) as f64).min(1.0));
    let (ow, oh) = (((bw as f64 * factor).ceil() as i32).max(1), ((bh as f64 * factor).ceil() as i32).max(1));
    // Output pixel -> shape (document) -> unit square -> source pixel, with a flipped layer's pixels mirrored.
    let Some(inverse) = invert(&homography(c)) else { bail!("The shape cannot be mapped.") };
    let (sw, sh) = (image.width() as usize, image.height() as usize);
    let (flip_x, flip_y) = (transform.flip_x, transform.flip_y);
    let out = if is_mask { crate::raster::a8_filled(ow, oh, 0)? } else { new_argb(ow, oh)? };
    let channels = if is_mask { 1 } else { 4 };
    let source: Vec<u8> = with_bytes(image, |d, stride| (0..sh).flat_map(|y| d[y * stride..y * stride + sw * channels].to_vec()).collect())?;
    let ostride = out.stride() as usize;
    let mut rows = vec![0u8; ostride * oh as usize];
    let sample = |u: f64, v: f64| -> [f64; 4] {
        let u = if flip_x { 1.0 - u } else { u };
        let v = if flip_y { 1.0 - v } else { v };
        let (x, y) = (u * sw as f64 - 0.5, v * sh as f64 - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let mut acc = [0.0; 4];
        for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
            for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                let (sx, sy) = (x0 as i64 + dx, y0 as i64 + dy);
                if sx < 0 || sy < 0 || sx >= sw as i64 || sy >= sh as i64 { continue; }
                let i = (sy as usize * sw + sx as usize) * channels;
                let w = wx * wy;
                for k in 0..channels { acc[k] += source[i + k] as f64 * w; }
            }
        }
        acc
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(16);
    let band = (oh as usize).div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (t, chunk) in rows.chunks_mut(ostride * band).enumerate() {
            let sample = &sample;
            scope.spawn(move || {
                let y_start = t * band;
                for (r, row) in chunk.chunks_mut(ostride).enumerate() {
                    let y = y_start + r;
                    let dy = by as f64 + (y as f64 + 0.5) / factor;
                    for x in 0..ow as usize {
                        let dx = bx as f64 + (x as f64 + 0.5) / factor;
                        let (u, v) = apply(&inverse, (dx, dy));
                        if !(-0.002..=1.002).contains(&u) || !(-0.002..=1.002).contains(&v) { continue; }
                        let acc = sample(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
                        for k in 0..channels { row[x * channels + k] = acc[k].round().clamp(0.0, 255.0) as u8; }
                    }
                }
            });
        }
    });
    with_bytes_raw_mut(&out, |d, _| d.copy_from_slice(&rows))?;
    Ok((out, placed))
}

/// A full-resolution warp cropped to its visible pixels, with the crop (in the warp's pixels) so a mask can be
/// cropped to match (`warpTrimmed`).
pub fn warp_trimmed(image: &ImageSurface, transform: &Transform, c: &Corners) -> Result<(ImageSurface, Transform, (i32, i32, i32, i32))> {
    let (warped, placed) = warp(image, transform, c, false, None)?;
    let (w, h) = (warped.width(), warped.height());
    let bounds = with_bytes(&warped, |d, stride| crate::ffi::alpha_bounds(d, w as usize, h as usize, stride))?;
    let Some((x0, y0, x1, y1)) = bounds else { return Ok((warped, placed, (0, 0, w, h))) };
    let (cw, ch) = ((x1 - x0) as i32, (y1 - y0) as i32);
    if cw < 1 || ch < 1 || (x0, y0, cw, ch) == (0, 0, w, h) { return Ok((warped, placed, (0, 0, w, h))); }
    let cropped = crop(&warped, x0 as i32, y0 as i32, cw, ch, false)?;
    let mut moved = placed;
    moved.origin = Point(placed.origin.0 + x0 as f64, placed.origin.1 + y0 as f64);
    moved.size = Size(cw as f64, ch as f64);
    Ok((cropped, moved, (x0 as i32, y0 as i32, cw, ch)))
}

pub fn crop(source: &ImageSurface, x: i32, y: i32, w: i32, h: i32, is_mask: bool) -> Result<ImageSurface> {
    let out = if is_mask { crate::raster::a8_filled(w, h, 0)? } else { new_argb(w, h)? };
    let cr = cairo::Context::new(&out)?;
    cr.set_source_surface(source, -(x as f64), -(y as f64))?;
    cr.set_operator(cairo::Operator::Source);
    cr.paint()?;
    drop(cr);
    Ok(out)
}

/// A mask warped like `warp`, but `background` (its tone past its pixels) outside the shape instead of black,
/// for masks placed apart from their layers (`warpMask`).
pub fn warp_mask(image: &ImageSurface, transform: &Transform, c: &Corners, background: u8, limit: Option<f64>) -> Result<(ImageSurface, Transform)> {
    let (warped, placed) = warp(image, transform, c, true, limit)?;
    if background == 0 || (image.width() == 1 && image.height() == 1) { return Ok((warped, placed)); }
    let (w, h) = (warped.width(), warped.height());
    let out = crate::raster::a8_filled(w, h, background)?;
    {
        let cr = cairo::Context::new(&out)?;
        let (sx, sy) = (w as f64 / placed.size.0, h as f64 / placed.size.1);
        cr.move_to((c[0].0 - placed.origin.0) * sx, (c[0].1 - placed.origin.1) * sy);
        for p in &c[1..] { cr.line_to((p.0 - placed.origin.0) * sx, (p.1 - placed.origin.1) * sy); }
        cr.close_path();
        cr.clip();
        cr.set_source_surface(&warped, 0.0, 0.0)?;
        cr.set_operator(cairo::Operator::Source);
        cr.paint()?;
    }
    Ok((out, placed))
}

/// A selection outline carried into the distorted shape: the mask of document pixels is mapped by the
/// perspective (`mapPath`), rendered through the same warp as the pixels.
pub fn warp_selection(selection: &crate::selection::Selection, pixel_transform: &Transform, c: &Corners) -> Result<crate::selection::Selection> {
    let (w, h) = (selection.width(), selection.height());
    let (warped, placed) = warp(&selection.mask, pixel_transform, c, true, None)?;
    let mask = crate::raster::a8_filled(w, h, 0)?;
    {
        let cr = cairo::Context::new(&mask)?;
        cr.set_source_surface(&warped, placed.origin.0, placed.origin.1)?;
        cr.paint()?;
    }
    crate::selection::Selection::from_mask(mask, selection.antialiased)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn homography_maps_corners_and_inverts() {
        let c: Corners = [(10.0, 10.0), (50.0, 12.0), (48.0, 40.0), (8.0, 44.0)];
        assert!(is_usable(&c));
        let m = homography(&c);
        for (i, u) in [(0, (0.0, 0.0)), (1, (1.0, 0.0)), (2, (1.0, 1.0)), (3, (0.0, 1.0))] {
            let p = apply(&m, u);
            assert!((p.0 - c[i].0).abs() < 1e-9 && (p.1 - c[i].1).abs() < 1e-9, "{i}: {p:?}");
        }
        let inv = invert(&m).unwrap();
        let back = apply(&inv, apply(&m, (0.3, 0.7)));
        assert!((back.0 - 0.3).abs() < 1e-9 && (back.1 - 0.7).abs() < 1e-9);
        assert!(!is_usable(&[(0.0, 0.0), (10.0, 10.0), (10.0, 0.0), (0.0, 10.0)]), "a bow tie");
        assert!(!is_usable(&[(0.0, 0.0), (10.0, 0.0), (10.0, 0.0), (0.0, 0.0)]), "collapsed");
    }
}
