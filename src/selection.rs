//! A selection is coverage over the document: an A8 mask at document size, 255 selected, with its outline
//! traced along pixel edges for the marching ants. The reference keeps a vector path and combines paths with
//! boolean operations; a mask makes those combinations plain per-pixel arithmetic and is what edits need anyway.

use crate::ffi;
use crate::format::Transform;
use crate::raster::{a8_filled, a8_from_data, with_bytes};
use crate::render::pixel_to_document;
use anyhow::Result;
use cairo::{Context, Extend, Filter, Format, ImageSurface, Matrix, Operator, SurfacePattern};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Replace, Add, Subtract }

#[derive(Clone)]
pub struct Selection {
    pub mask: ImageSurface,
    /// Closed loops of pixel-edge corners; empty when the outline is too detailed to draw.
    pub outline: Rc<Vec<Vec<(i32, i32)>>>,
    /// Half-open (left, top, right, bottom) of the selected pixels; None for an explicit empty selection.
    pub bounds: Option<(usize, usize, usize, usize)>,
    pub antialiased: bool,
}

impl PartialEq for Selection {
    fn eq(&self, other: &Self) -> bool { self.mask.to_raw_none() == other.mask.to_raw_none() }
}

impl Selection {
    pub fn width(&self) -> i32 { self.mask.width() }
    pub fn height(&self) -> i32 { self.mask.height() }
    pub fn is_empty(&self) -> bool { self.bounds.is_none() }

    /// A selection from an A8 document-size mask, outlined.
    pub fn from_mask(mask: ImageSurface, antialiased: bool) -> Result<Selection> {
        let (w, h) = (mask.width() as usize, mask.height() as usize);
        let (bounds, outline) = with_bytes(&mask, |data, stride| -> Result<_> {
            let bounds = ffi::gray_bounds(data, w, h, stride);
            let outline = match bounds {
                None => Some(Vec::new()),
                Some(_) => {
                    let mut packed = vec![0u8; w * h];
                    for y in 0..h { packed[y * w..(y + 1) * w].copy_from_slice(&data[y * stride..y * stride + w]); }
                    ffi::trace(&packed, w, h)?
                }
            };
            Ok((bounds, outline.unwrap_or_default()))
        })??;
        Ok(Selection { mask, outline: Rc::new(outline), bounds, antialiased })
    }

    /// A packed width x height byte mask as the wand writes it.
    pub fn from_packed(packed: &[u8], width: i32, height: i32) -> Result<Selection> {
        let (w, h) = (width as usize, height as usize);
        let stride = Format::A8.stride_for_width(width as u32)? as usize;
        let mut data = vec![0u8; stride * h];
        for y in 0..h { data[y * stride..y * stride + w].copy_from_slice(&packed[y * w..(y + 1) * w]); }
        Selection::from_mask(a8_from_data(width, height, data, stride as i32)?, true)
    }

    /// Everything.
    pub fn all(width: i32, height: i32) -> Result<Selection> {
        Selection::from_mask(a8_filled(width, height, 255)?, true)
    }

    /// The whole document rasterized from `draw`, which fills its shape into an A8 context in document pixels.
    pub fn from_shape(width: i32, height: i32, antialiased: bool, draw: impl FnOnce(&Context) -> Result<()>) -> Result<Selection> {
        let mask = a8_filled(width, height, 0)?;
        {
            let cr = Context::new(&mask)?;
            cr.set_antialias(if antialiased { cairo::Antialias::Default } else { cairo::Antialias::None });
            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
            draw(&cr)?;
        }
        Selection::from_mask(mask, antialiased)
    }

    /// This selection combined with `other`: replaced, unioned (max) or subtracted (min with the inverse).
    pub fn combined(&self, other: &Selection, mode: Mode) -> Result<Selection> {
        if mode == Mode::Replace { return Ok(other.clone()); }
        let (w, h) = (self.width() as usize, self.height() as usize);
        let stride = self.mask.stride() as usize;
        let mut data = with_bytes(&self.mask, |d, _| d.to_vec())?;
        with_bytes(&other.mask, |o, ostride| {
            for y in 0..h {
                for x in 0..w {
                    let a = &mut data[y * stride + x];
                    let b = o[y * ostride + x];
                    *a = match mode { Mode::Add => (*a).max(b), _ => (*a).min(255 - b) };
                }
            }
        })?;
        Selection::from_mask(a8_from_data(self.width(), self.height(), data, stride as i32)?, self.antialiased && other.antialiased)
    }

    pub fn inverted(&self) -> Result<Selection> {
        let stride = self.mask.stride() as usize;
        let data = with_bytes(&self.mask, |d, _| d.iter().map(|v| 255 - v).collect::<Vec<u8>>())?;
        Selection::from_mask(a8_from_data(self.width(), self.height(), data, stride as i32)?, self.antialiased)
    }

    /// The selection moved by whole pixels; what leaves the canvas is lost.
    pub fn translated(&self, dx: i32, dy: i32) -> Result<Selection> {
        let mask = a8_filled(self.width(), self.height(), 0)?;
        {
            let cr = Context::new(&mask)?;
            cr.set_source_surface(&self.mask, dx as f64, dy as f64)?;
            cr.source().set_filter(Filter::Nearest);
            cr.set_operator(Operator::Source);
            cr.paint()?;
        }
        Selection::from_mask(mask, self.antialiased)
    }

    /// Coverage on a layer's own `width` x `height` pixel grid, placed on the document by `transform`
    /// (`PixelAdjust.coverage`): what an edit blends its result through.
    pub fn coverage_on_layer(&self, transform: &Transform, width: i32, height: i32) -> Result<ImageSurface> {
        let surface = a8_filled(width, height, 0)?;
        {
            let cr = Context::new(&surface)?;
            let to_document = pixel_to_document(transform, width, height);
            let pattern = SurfacePattern::create(&self.mask);
            // The pattern is sampled in layer pixels: map them onto the document, where the mask lives.
            pattern.set_matrix(to_document);
            pattern.set_filter(Filter::Good);
            pattern.set_extend(Extend::None);
            cr.set_source(&pattern)?;
            cr.set_operator(Operator::Source);
            cr.paint()?;
        }
        Ok(surface)
    }

    /// Coverage over the rectangle (x, y, w, h) of a grid that `to_document` places on the document, packed
    /// one byte per pixel: what a stroke's tile multiplies its coverage by.
    pub fn coverage_on_grid(&self, to_document: &Matrix, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; w * h];
        let Ok(surface) = a8_filled(w as i32, h as i32, 0) else { return out };
        let drawn = (|| -> Result<()> {
            let cr = Context::new(&surface)?;
            let pattern = SurfacePattern::create(&self.mask);
            pattern.set_matrix(Matrix::multiply(&Matrix::new(1.0, 0.0, 0.0, 1.0, x as f64, y as f64), to_document));
            pattern.set_filter(Filter::Good);
            pattern.set_extend(Extend::None);
            cr.set_source(&pattern)?;
            cr.set_operator(Operator::Source);
            cr.paint()?;
            Ok(())
        })();
        if drawn.is_err() { return out; }
        let _ = with_bytes(&surface, |data, stride| { for r in 0..h { out[r * w..(r + 1) * w].copy_from_slice(&data[r * stride..r * stride + w]); } });
        out
    }

    /// Whether the document pixel at (x, y) is at least half selected.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        if x < 0.0 || y < 0.0 || x >= self.width() as f64 || y >= self.height() as f64 { return false; }
        with_bytes(&self.mask, |d, stride| d[y as usize * stride + x as usize] >= 128).unwrap_or(false)
    }
}

/// The matrix cairo needs so a document-space mask samples correctly in layer pixels; kept for callers that
/// draw the selection themselves.
pub fn document_to_layer(transform: &Transform, width: i32, height: i32) -> Result<Matrix> {
    Ok(pixel_to_document(transform, width, height).try_invert()?)
}

/// Squared Euclidean distance transform of a binary image (Felzenszwalb and Huttenlocher), distance from
/// each pixel to the nearest pixel where `inside` is true.
fn distance_squared(inside: &[bool], w: usize, h: usize) -> Vec<f64> {
    const INF: f64 = 1e20;
    fn transform_1d(f: &[f64], out: &mut [f64], v: &mut [usize], z: &mut [f64]) {
        let n = f.len();
        let mut k = 0usize;
        v[0] = 0;
        z[0] = -INF;
        z[1] = INF;
        for q in 1..n {
            let mut s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64)) / (2.0 * q as f64 - 2.0 * v[k] as f64);
            while s <= z[k] {
                k -= 1;
                s = ((f[q] + (q * q) as f64) - (f[v[k]] + (v[k] * v[k]) as f64)) / (2.0 * q as f64 - 2.0 * v[k] as f64);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = INF;
        }
        k = 0;
        for q in 0..n {
            while z[k + 1] < q as f64 { k += 1; }
            out[q] = (q as f64 - v[k] as f64).powi(2) + f[v[k]];
        }
    }
    let mut d: Vec<f64> = inside.iter().map(|&b| if b { 0.0 } else { INF }).collect();
    let n = w.max(h);
    let (mut f, mut out, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h { f[y] = d[y * w + x]; }
        transform_1d(&f[..h], &mut out[..h], &mut v[..h], &mut z[..h + 1]);
        for y in 0..h { d[y * w + x] = out[y]; }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&d[y * w..(y + 1) * w]);
        transform_1d(&f[..w], &mut out[..w], &mut v[..w], &mut z[..w + 1]);
        d[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    d
}

impl Selection {
    /// The selection grown (`amount` > 0) or shrunk (`amount` < 0) by that many pixels with round corners, as
    /// Photoshop's Expand and Contract do: pixels within the distance of the outline change side.
    pub fn resized(&self, amount: i32) -> Result<Selection> {
        if amount == 0 { return Ok(self.clone()); }
        let (w, h) = (self.width() as usize, self.height() as usize);
        let stride = self.mask.stride() as usize;
        let selected: Vec<bool> = with_bytes(&self.mask, |d, _| (0..w * h).map(|i| d[(i / w) * stride + i % w] >= 128).collect())?;
        let radius = (amount.abs() as f64).powi(2) + 0.25;
        let mut data = vec![0u8; stride * h];
        if amount > 0 {
            let d = distance_squared(&selected, w, h);
            for i in 0..w * h { if d[i] <= radius { data[(i / w) * stride + i % w] = 255; } }
        } else {
            let outside: Vec<bool> = selected.iter().map(|s| !s).collect();
            let d = distance_squared(&outside, w, h);
            // Contracting also pulls away from the canvas edges.
            for i in 0..w * h {
                let (x, y) = (i % w, i / w);
                let edge = (x.min(w - 1 - x).min(y).min(h - 1 - y) + 1) as f64;
                if selected[i] && d[i] > radius && edge * edge > radius { data[y * stride + x] = 255; }
            }
        }
        Selection::from_mask(a8_from_data(self.width(), self.height(), data, stride as i32)?, self.antialiased)
    }
}
